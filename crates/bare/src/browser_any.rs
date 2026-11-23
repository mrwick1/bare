//! A browser on either engine. `BARE_JS_EXEC=new` makes the pages of a session on js2 (`Browser<New>`), every other value on the old
//! engine (`Browser<Old>`). The layers above the DOM host (`Session`, `Page`, the driver) hold this enum and call the methods that
//! they need. A method forwards to the browser of the variant. Its result has no type of the engine in it: a fault is an `AnyFault`.

use crate::browser::{Browser, FileRef, Report, Survive};
use crate::browser_console::ConsoleLevel;
use crate::browser_focus::FocusOutcome;
use crate::browser_loader::{Input, PageLimits, PageResult};
use crate::browser_navigation::NavigationRequest;
use crate::browser_scroll::ScrollKey;
use crate::browser_ui::{Frame, KeyInput, Pointer, UiOutcome};
use crate::downloads::DownloadRequest;
use crate::eng::{Eng, Fault, Ops, PState, Val};
use bare_js_api::Persistent;
use crate::eng_new::New;
use crate::eng_old::Old;
use bare_css::cascade::{Change, Styles};
use bare_html::dom::{Document, NodeId};
use bare_layout::fragment::Rect;
use bare_net::http::Client;
use bare_paint::Scroll;
use std::cell::{Ref, RefCell};
use std::rc::Rc;

pub enum AnyBrowser {
    Old(Browser<Old>),
    New(Browser<New>),
}

/// A fault of either engine.
#[derive(Clone, Copy, Debug)]
pub enum AnyFault {
    Old(Fault<Old>),
    New(Fault<New>),
}

pub type AR<T> = Result<T, AnyFault>;

impl AnyFault {
    /// A style sheet that is not supplied yet (`browser_computed::is_missing_sheet`).
    pub fn is_missing_sheet(&self) -> bool {
        match self {
            AnyFault::Old(e) => crate::browser_computed::is_missing_sheet::<Old>(e),
            AnyFault::New(e) => crate::browser_computed::is_missing_sheet::<New>(e),
        }
    }
}

/// The console records and what a driver reads of a script value, with no type of the engine in them.
fn console<E: crate::eng::Eng>(b: &mut Browser<E>) -> Vec<(&'static str, String)> {
    b.take_console_records()
        .into_iter()
        .map(|r| {
            let level = match r.level {
                ConsoleLevel::Log => "log",
                ConsoleLevel::Warn => "warn",
                ConsoleLevel::Error => "error",
            };
            (level, b.format_console_record(&r).unwrap_or_else(|e| format!("(not formatted: {e:?})")))
        })
        .collect()
}

/// `String(v[name])`, or `?` when it fails (`Interp::get_str`).
fn get_str<E: Eng>(b: &mut Browser<E>, v: E::Raw, name: &str) -> String {
    let r = (|| {
        let key = b.it.h_key(name)?;
        let x = b.it.get_value(v, key)?;
        let s = b.it.to_js_string(x)?;
        Ok::<_, Fault<E>>(b.it.display_o(E::str_raw(s)))
    })();
    r.unwrap_or_else(|_| "?".into())
}

/// A thrown value as text: `Name: message` for an error object, else its string form.
fn thrown_value<E: Eng>(b: &mut Browser<E>, v: E::Raw) -> String {
    if matches!(E::kind(v), Val::Object(_)) {
        let (n, m) = (get_str(b, v, "name"), get_str(b, v, "message"));
        if n != "?" && !(n == "undefined" && m == "undefined") {
            return if m.is_empty() || m == "undefined" { n } else { format!("{n}: {m}") };
        }
    }
    b.it.display_o(v)
}

fn thrown<E: Eng>(b: &mut Browser<E>, e: &Fault<E>) -> String {
    match e {
        Fault::Throw(v) => thrown_value(b, *v),
        e => format!("{e:?}"),
    }
}

/// One look at a script value that a driver waits on (`AnyBrowser::promise_step`).
pub enum Step {
    /// Not a promise, or a promise that is done: the value as text.
    Done(String),
    /// A fulfilled promise: go on with its value.
    Next(Persistent),
    Rejected(String),
    Pending,
}

fn step<E: Eng>(b: &mut Browser<E>, p: &Persistent, cap: usize) -> Step {
    let v = b.it.persistent_value(p);
    match b.it.promise_status(v) {
        None => Step::Done(b.inspect(v, cap)),
        Some(PState::Fulfilled(x)) => Step::Next(b.it.persist_value(x)),
        Some(PState::Rejected(x)) => Step::Rejected(thrown_value(b, x)),
        Some(PState::Pending) => Step::Pending,
    }
}

macro_rules! fwd {
    ($( ($($rcv:tt)+) fn $n:ident($($a:ident: $t:ty),*) $(-> $r:ty)?; )*) => {
        impl AnyBrowser {
            $( pub fn $n($($rcv)+ self $(, $a: $t)*) $(-> $r)? {
                match self { AnyBrowser::Old(b) => b.$n($($a),*), AnyBrowser::New(b) => b.$n($($a),*) }
            } )*
        }
    };
}

/// The same for a method whose result is `R<E, T>`: the fault is an `AnyFault`.
macro_rules! fwd_try {
    ($( ($($rcv:tt)+) fn $n:ident($($a:ident: $t:ty),*) -> $r:ty; )*) => {
        impl AnyBrowser {
            $( pub fn $n($($rcv)+ self $(, $a: $t)*) -> AR<$r> {
                match self { AnyBrowser::Old(b) => b.$n($($a),*).map_err(AnyFault::Old), AnyBrowser::New(b) => b.$n($($a),*).map_err(AnyFault::New) }
            } )*
        }
    };
}

fwd! {
    (&) fn blocks_settle(limit: f64) -> bool;
    (&) fn settle_blocker(limit: f64) -> Option<(String, f64)>;
    (&) fn document() -> Rc<RefCell<Document>>;
    (&) fn document_url() -> Option<String>;
    (&) fn fetches_since(t: std::time::Instant) -> usize;
    (&) fn has_children(node: NodeId) -> bool;
    (&) fn has_download_request() -> bool;
    (&) fn img_requested() -> bool;
    (&) fn img_requests() -> Vec<NodeId>;
    (&) fn img_settled(n: NodeId, ok: bool);
    (&) fn img_started(n: NodeId);
    (&) fn navigation_stats() -> crate::browser_navigation::NavigationStats;
    (&) fn listened(types: &[&str]) -> std::collections::HashSet<NodeId>;
    (&) fn sched_stats() -> crate::browser_scheduler::SchedStats;
    (&) fn stats() -> crate::browser::BrowserStats;
    (&) fn describe(node: NodeId) -> String;
    (&mut) fn inline_rects(node: NodeId) -> Vec<Rect>;
    (&) fn input_checked(n: NodeId) -> bool;
    (&) fn input_selection(input: NodeId) -> Option<(u32, u32, &'static str)>;
    (&) fn input_value(input: NodeId) -> Option<String>;
    (&) fn is_editing_host(node: NodeId) -> bool;
    (&) fn is_text_input(node: NodeId) -> bool;
    (&) fn date_kind(node: NodeId) -> Option<&'static str>;
    (&) fn live() -> bool;
    (&) fn live_work() -> Option<bool>;
    (&) fn live_work_of(streams: bool) -> Option<bool>;
    (&) fn loaded() -> bool;
    (&) fn next_scheduled_due() -> Option<f64>;
    (&) fn now() -> f64;
    (&) fn pending_fetches() -> usize;
    (&) fn pin(n: NodeId);
    (&) fn ready_level() -> u8;
    (&) fn render_key() -> [u64; 8];
    (&) fn resolve_url(input: &str) -> Result<String, String>;
    (&mut) fn restyle_frame(f: &mut Frame, styles: Rc<Styles>);
    (&mut) fn resume_page(client: Client, limits: PageLimits, steps: &[Input]) -> PageResult;
    (&) fn scroll_pending() -> bool;
    (&) fn selected_labels(select: NodeId) -> Option<Vec<String>>;
    (&) fn selected_option(select: NodeId) -> Option<NodeId>;
    (&) fn selecting() -> Option<NodeId>;
    (&mut) fn set_scroll(s: Scroll);
    (&mut) fn take_download_request() -> Option<DownloadRequest>;
    (&mut) fn take_fetch_diagnostics() -> Vec<String>;
    (&mut) fn take_navigation_request() -> Option<NavigationRequest>;
    (&mut) fn take_reports() -> Vec<Report>;
    (&mut) fn take_scroll_key() -> Option<ScrollKey>;
    (&) fn tasks_held() -> bool;
    (&) fn waiting_loads(out: &mut Vec<crate::page::Waiting>);
    (&) fn waiting_requests(out: &mut Vec<crate::page::Waiting>);
}

fwd_try! {
    (&mut) fn active_element() -> Option<NodeId>;
    (&mut) fn drive_select(node: NodeId, wanted: &[String]) -> Result<UiOutcome, String>;
    (&mut) fn drive_value(node: NodeId, value: String) -> UiOutcome;
    (&mut) fn focus(node: NodeId) -> FocusOutcome;
    (&mut) fn hide() -> ();
    (&mut) fn history_go(delta: i32) -> ();
    (&mut) fn navigate_same_document(url: &str) -> bool;
    (&mut) fn query(selector: &str) -> Option<NodeId>;
    (&mut) fn query_all(selector: &str) -> Vec<NodeId>;
    (&mut) fn refresh(f: &mut Frame) -> Option<Change>;
    (&mut) fn render_with(styles: Rc<Styles>, old: Option<Frame>) -> Frame;
    (&mut) fn restyle() -> Rc<Styles>;
    (&mut) fn set_viewport(width: f32, height: f32) -> usize;
    (&mut) fn ui_drag_select(caret: u32) -> UiOutcome;
    (&mut) fn ui_insert_text(text: &str) -> UiOutcome;
    (&mut) fn ui_key(k: KeyInput, down: bool) -> UiOutcome;
    (&mut) fn ui_pointer_button(target: Option<NodeId>, p: Pointer, button: u8, pressed: bool, caret: Option<u32>) -> UiOutcome;
    (&mut) fn ui_pointer_leave(p: Pointer) -> UiOutcome;
    (&mut) fn ui_pointer_move(target: Option<NodeId>, p: Pointer) -> UiOutcome;
    (&mut) fn ui_pointer_settle(to: Option<NodeId>, p: Pointer) -> UiOutcome;
    (&mut) fn ui_scroll_events() -> UiOutcome;
}

impl AnyBrowser {
    pub(crate) fn drive_files(&mut self, node: NodeId, files: Vec<FileRef>) -> AR<UiOutcome> {
        match self {
            AnyBrowser::Old(b) => b.drive_files(node, files).map_err(AnyFault::Old),
            AnyBrowser::New(b) => b.drive_files(node, files).map_err(AnyFault::New),
        }
    }

    pub fn scroll(&self) -> Ref<'_, Scroll> {
        match self {
            AnyBrowser::Old(b) => b.scroll(),
            AnyBrowser::New(b) => b.scroll(),
        }
    }

    /// Does a script budget end only the task (`Browser::survive`)? `e` is a fault of this browser.
    pub fn survive(&mut self, e: AnyFault, what: &str) -> Survive {
        match (self, e) {
            (AnyBrowser::Old(b), AnyFault::Old(e)) => b.survive(e, what),
            (AnyBrowser::New(b), AnyFault::New(e)) => b.survive(e, what),
            _ => Survive::Fatal,
        }
    }

    /// The console calls since the last call, as a level and the text.
    pub fn take_console(&mut self) -> Vec<(&'static str, String)> {
        match self {
            AnyBrowser::Old(b) => console(b),
            AnyBrowser::New(b) => console(b),
        }
    }

    /// Run the queued work of a bare browser (a test, no session): the microtasks, the navigation tasks and the timers.
    pub fn run_tasks(&mut self) -> AR<()> {
        match self {
            AnyBrowser::Old(b) => b.drain().and_then(|_| b.run_navigation_tasks()).and_then(|_| b.run_scheduled_tasks(1000)).map(|_| ()).map_err(AnyFault::Old),
            AnyBrowser::New(b) => b.drain().and_then(|_| b.run_navigation_tasks()).and_then(|_| b.run_scheduled_tasks(1000)).map(|_| ()).map_err(AnyFault::New),
        }
    }

    /// Is the control `node` disabled (`Dom::is_disabled_control`)?
    pub fn is_disabled_control(&mut self, node: NodeId) -> bool {
        fn go<E: crate::eng::Eng>(b: &mut Browser<E>, node: NodeId) -> bool {
            let dom = b.dom.clone();
            b.it.host_scope_o(|it| dom.is_disabled_control(it, node)).unwrap_or(false)
        }
        match self {
            AnyBrowser::Old(b) => go(b, node),
            AnyBrowser::New(b) => go(b, node),
        }
    }

    /// Forget the last pointer target of the interface (`ui.last`).
    pub fn forget_pointer(&mut self) {
        match self {
            AnyBrowser::Old(b) => b.dom.st.borrow_mut().ui.last = None,
            AnyBrowser::New(b) => b.dom.st.borrow_mut().ui.last = None,
        }
    }

    /// Run `source` as a script. The value stays alive in the realm until the handle is dropped. The text of a script that threw is in the fault
    /// (`fault_text`).
    pub fn script(&mut self, source: &str) -> AR<Persistent> {
        match self {
            AnyBrowser::Old(b) => b.run_script(source).map(|v| b.it.persist_value(v)).map_err(AnyFault::Old),
            AnyBrowser::New(b) => b.run_script(source).map(|v| b.it.persist_value(v)).map_err(AnyFault::New),
        }
    }

    /// Run `source` and give its value as text, and whether it is a string.
    pub fn script_text(&mut self, source: &str) -> AR<(bool, String)> {
        fn go<E: Eng>(b: &mut Browser<E>, source: &str) -> Result<(bool, String), Fault<E>> {
            let v = b.run_script(source)?;
            Ok((matches!(E::kind(v), Val::String(_)), b.it.display_o(v)))
        }
        match self {
            AnyBrowser::Old(b) => go(b, source).map_err(AnyFault::Old),
            AnyBrowser::New(b) => go(b, source).map_err(AnyFault::New),
        }
    }

    /// Does `source` give `true`?
    pub fn script_true(&mut self, source: &str) -> AR<bool> {
        fn go<E: Eng>(b: &mut Browser<E>, source: &str) -> Result<bool, Fault<E>> {
            let v = b.run_script(source)?;
            Ok(matches!(E::kind(v), Val::Bool(true)))
        }
        match self {
            AnyBrowser::Old(b) => go(b, source).map_err(AnyFault::Old),
            AnyBrowser::New(b) => go(b, source).map_err(AnyFault::New),
        }
    }

    /// A fault of this browser as text: `Name: message` for a thrown error object.
    pub fn fault_text(&mut self, e: &AnyFault) -> String {
        match (self, e) {
            (AnyBrowser::Old(b), AnyFault::Old(e)) => thrown(b, e),
            (AnyBrowser::New(b), AnyFault::New(e)) => thrown(b, e),
            _ => format!("{e:?}"),
        }
    }

    /// Look at a script value once: its text, the value of a fulfilled promise, a rejection, or a promise that is pending.
    pub fn promise_step(&mut self, v: &Persistent, cap: usize) -> Step {
        match self {
            AnyBrowser::Old(b) => step(b, v, cap),
            AnyBrowser::New(b) => step(b, v, cap),
        }
    }

    /// The jobs of the microtask queue that wait.
    pub fn pending_jobs(&self) -> usize {
        match self {
            AnyBrowser::Old(b) => b.it.pending_jobs(),
            AnyBrowser::New(b) => b.it.pending_jobs(),
        }
    }
}

/// The variant of a browser, to run one expression on both: `any!(b, x => x.method())`.
#[macro_export]
macro_rules! any {
    ($b:expr, $x:ident => $e:expr) => {
        match $b {
            $crate::browser_any::AnyBrowser::Old($x) => $e,
            $crate::browser_any::AnyBrowser::New($x) => $e,
        }
    };
}

/// The old-engine view of a browser, for the tests and the examples, which run on the old engine. It panics on js2. The code of the
/// product calls the methods of `AnyBrowser`.
#[doc(hidden)]
impl std::ops::Deref for AnyBrowser {
    type Target = Browser<Old>;
    fn deref(&self) -> &Browser<Old> {
        match self {
            AnyBrowser::Old(b) => b,
            AnyBrowser::New(_) => panic!("this browser runs on js2"),
        }
    }
}

#[doc(hidden)]
impl std::ops::DerefMut for AnyBrowser {
    fn deref_mut(&mut self) -> &mut Browser<Old> {
        match self {
            AnyBrowser::Old(b) => b,
            AnyBrowser::New(_) => panic!("this browser runs on js2"),
        }
    }
}

impl From<Browser<Old>> for AnyBrowser {
    fn from(b: Browser<Old>) -> Self {
        AnyBrowser::Old(b)
    }
}

impl From<Browser<New>> for AnyBrowser {
    fn from(b: Browser<New>) -> Self {
        AnyBrowser::New(b)
    }
}
