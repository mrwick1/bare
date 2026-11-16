//! Structured failures for `tools/call` results: a stable `code`, redacted evidence and a conservative `next_action`.
//!
//! The code comes from the source of the failure: the `kind` of a driver `Fail`, or the place in this server that rejected the call
//! (argument check, tool name, session lookup). Text is never parsed to find a code. The advice never says to repeat an action: a click,
//! a type, an upload or a navigation may have run before it failed, and only the agent can decide after it looks at the page.

use bare_drive::run::Redactor;
use serde_json::{Value, json};

/// A tool failure with its source-derived code. `kind: None` is an error with no known source: it gets the code `internal`.
pub struct Failure {
    pub text: String,
    pub code: Option<&'static str>,
    /// The driver's own message when `text` is a longer receipt that holds it.
    pub detail: Option<String>,
    /// Whether the input of the call may have reached the page: `no` (rejected before the dispatch) or `unknown`.
    pub input: &'static str,
}

impl Failure {
    /// A call rejected before it ran: bad arguments.
    pub fn args(text: impl Into<String>) -> Failure {
        Failure { text: text.into(), code: Some("invalid_arguments"), detail: None, input: "no" }
    }
    pub fn unknown_tool(name: &str) -> Failure {
        Failure { text: format!("unknown tool {name}"), code: Some("unknown_tool"), detail: None, input: "no" }
    }
    pub fn no_session(name: &str) -> Failure {
        Failure { text: format!("no live session {name}: call open first"), code: Some("missing_session"), detail: None, input: "no" }
    }
    /// A failed step of the driver: `kind` is `Fail.kind`. The input may have run before the failure, so that stays unknown.
    pub fn driver(kind: &'static str, detail: String, receipt: String) -> Failure {
        Failure { text: receipt, code: Some(kind), detail: Some(detail), input: "unknown" }
    }
}

/// An untyped error (a text from a tool that has no typed source yet) gets the fallback code.
impl From<String> for Failure {
    fn from(text: String) -> Failure {
        Failure { text, code: None, detail: None, input: "unknown" }
    }
}
impl From<&str> for Failure {
    fn from(text: &str) -> Failure {
        text.to_string().into()
    }
}
impl From<Failure> for String {
    fn from(f: Failure) -> String {
        f.text
    }
}

/// The action for a code. One of: `inspect_page`, `refresh_observation`, `refine_target`, `dismiss_overlay`, `wait_for_condition`,
/// `open_session`, `diagnose`, `correct_arguments`. None repeats an input: each asks for a look, a new target or a wait first.
pub fn next_action(code: &str) -> &'static str {
    match code {
        "stale_ref" | "detached" => "refresh_observation",
        "duplicate" | "not_editable" | "no_focus" | "focus_lost" => "refine_target",
        "covered" => "dismiss_overlay",
        "disabled" => "wait_for_condition",
        "invalid_arguments" | "unknown_tool" => "correct_arguments",
        "missing_session" => "open_session",
        "not_found" | "hidden" | "expect" => "inspect_page",
        // page, cookie, state, shot and the fallback: the driver has no more specific fact, so look at the console and the network.
        _ => "diagnose",
    }
}

/// The `structuredContent` of a failed result: `{"failure": {code, message, next_action, input_dispatched}}`. `message` is the
/// driver or server text with the session secrets hidden and the URLs in it left as they are (the driver already applied `safe_url`).
pub fn structured(f: &Failure, red: &Redactor) -> Value {
    let code = f.code.unwrap_or("internal");
    let msg: String = red.clean(f.detail.as_deref().unwrap_or(&f.text)).chars().take(1000).collect();
    json!({"failure": {"code": code, "message": msg, "next_action": next_action(code), "input_dispatched": f.input}})
}
