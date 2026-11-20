//! Task scheduling of the browser host: timers, `queueMicrotask`, `performance.now` and `MessageChannel`.
//! See `browser.rs` for the host design. This file owns its own state (`Sched`) and hooks into `Dom` in four places
//! only: install, roots, trace and prune.
//!
//! Model
//! - One task queue, ordered by `(due time, sequence number)`. A timer task is due at `now + delay`. A message
//!   delivery task is due at `now` when it is queued. Equal due times run in queue order. There is no OS timer,
//!   thread or server: nothing runs unless the driver calls `Browser::run_scheduled_tasks` or `advance_to`.
//! - Clock: `Monotonic` (default, `Instant`) or `Virtual` (`Browser::use_virtual_clock`). A virtual clock moves only
//!   when the driver moves it, so tests never sleep. `performance.now()` and `Event.timeStamp` read the same clock.
//!   `advance_to(t)` steps the clock to each due task in order (a task takes zero virtual time), then to `t`.
//! - The wall clock of `Date.now()` is not this clock. It counts ms since 1970 (`Browser::wall_clock`, default `SystemTime`) and
//!   has its own virtual mode (`use_virtual_wall_clock_at`). Moving one never moves the other: a timer that fires at +1000 ms
//!   of virtual scheduler time sees the same `Date.now()` as before, unless the driver also sets the wall clock.
//! - A task runs in one `host_scope` (a fresh step budget, the normal call-depth guard), then a microtask checkpoint.
//!   Microtasks that the checkpoint leaves (a `DrainJobs` cap) run first in the next pump.
//! - `run_scheduled_tasks(max)` runs at most `max` due tasks and returns. It never sleeps. `Pump::next_due` says when
//!   the driver may call again. It is separate from any navigation or network pump.
//! - A listener or callback that throws is reported (`Browser::take_reports`) and the pump goes on. Any other error
//!   (a Limit, Unsupported, a heap fault) ends the pump and is sticky: later pumps return it and run no JS.
//!
//! Timers (spec: HTML "timer initialization steps")
//! - `setTimeout`/`setInterval(callback, delay, ...args)`. The delay is WebIDL `long`: `ToInt32`, then negative is 0.
//!   So `2**31` becomes `-2**31`, which becomes 0 (the implemented delay conversion). From the 6th level of nested timers a delay
//!   below 4 ms becomes 4 ms. A timer set from a message task has level 1.
//! - Ids are numbers from 1, never reused. `clearTimeout` and `clearInterval` share the cancellation algorithm. An interval
//!   that is cleared while its callback runs is not queued again. An interval is queued again after each run, at
//!   `clock when the run started + interval`, even when its callback throws.
//! - A string callback (it would be `eval`) and any other non-function is `Unsupported`.
//!
//! Recurring timers and the settle rule (rule: header of `browser_session.rs`). A `Timer` has `period` (the delay of an interval, or of a
//!   timeout that was set while its own callback ran; 0 if it is a one-shot or the delay is under 16 ms) and `idle` (its last run was
//!   quiet). `run_task` reads `settle_mark` before a timer or a rendering step and `quiet_since` after it: DOM changes (the
//!   `revision` and `text_revision` of the document, at most 8), queued fetches, scripts and traversals (no more than before),
//!   and the count of queued tasks (no new task but the re-arm; the rendering step and a message do not count, they are judged on their
//!   own and a noisy one makes the timers be judged again). Cost: three counter reads and three borrows for each timer, frame or message. `blocks_settle(limit)` walks the queue in due order up to `limit` and answers at the first task that `Sched::skips` is false for.
//!   The detection of a re-arm is the identity of the callback object (`Sched::running`), nothing else.
//!
//! `queueMicrotask(cb)` queues a real job in the VM microtask queue (`Interp::queue_microtask`). A throw is reported
//! (event type `microtask`) and later jobs run. A Limit or Unsupported ends the checkpoint (sticky, as for Promise jobs).
//!
//! `MessageChannel` / `MessagePort` / `MessageEvent`
//! - A port is an `EventTarget` (the common listener code runs). `onmessage` (the setter also starts the port),
//!   `addEventListener('message')`, `start()`, `close()`, `postMessage(message[, transfer])`.
//! - A message is queued in the receiving port. Before `start()` it waits there. After it, each message is one task.
//!   `close()` disentangles the pair: later posts from either port are dropped (no error). Messages that were
//!   queued before the close are still delivered, to both ports (the HTML Standard queues no check on the closed
//!   state, and Node behaves the same; not checked in a browser).
//! - Payloads use structured clone, including object graphs and arrays. Functions, symbols and host objects
//!   throw `DataCloneError`.
//! - Transfer list (HTML "StructuredSerializeWithTransfer"): `postMessage(message, [items])` or `{ transfer }`, any iterable. An item is a
//!   `MessagePort` or an `ArrayBuffer`, once, not detached and not the posting port; else `DataCloneError`. A buffer moves: the receiver
//!   holds its bytes and the source is detached. A port moves: the sent object becomes a shell (no peer, no queue; `postMessage` does
//!   nothing and it cannot be sent again). A new port object takes its peer link and its queued messages. It starts disabled, and
//!   its listeners stay with the old object. The data in a message is the heap-free `State` of `browser_navigation_state.rs`, and the
//!   items go in the `moved` list of the record (`Moved::Host` holds a `Transfer`). A port in the message but not in the list is `DataCloneError`.
//! - `MessageEvent` has `data`, `origin` (empty), `lastEventId` (empty), `source` (null) and `ports` (a frozen array of the new ports
//!   of the transfer list). It is not constructible. `isTrusted` is false (the Event getter), which differs from a browser.
//! - A port can move to another page (`Dom::export_ports`; `browser_port_hub.rs`). Then the pair is split: each port has an end in the hub
//!   (`Port::me`, and `Port::remote` for the peer). A post goes to the hub as a wire, and the page that owns the peer takes it
//!   (`Dom::take_wires`). `close()` removes the end: later posts to it are dropped. A message that waited in a port when it moved goes with
//!   it, in order. If that message carried ports, it is dropped with one console warning.
//! - Deviations: `port1` and `port2` are own read-only properties of the channel, not
//!   prototype getters. A handler set with `onmessage` and an `addEventListener` with the same function count as one
//!   listener. `onmessageerror`, the `close` event and `BroadcastChannel` are absent. `window.postMessage` is in `browser_basics.rs`.
//!
//! `requestAnimationFrame` and the rendering step are in `browser_frame.rs`. `requestIdleCallback` is in `browser_idle.rs`: an idle
//! period is `Task::Idle`, queued only by a request. It runs after every task that is due, and after the frame that is queued; its
//! deadline is `min(50 ms, next task)`. `Task::IdleTimeout` runs a callback whose `timeout` passed. Not here: `setImmediate`, an `error` event or `onerror` for
//! reported exceptions, `ErrorEvent`.
//!
//! GC: timers keep their callbacks and arguments in persistent handles of `bare-js-api` (the engine roots them; a cleared or finished
//! timer drops them). A port that has queued delivery tasks is a root. So is an enabled port
//! with a peer in another page (a message can come at any time). `sched_prune` removes the end of a port that the GC drops.
//! A port keeps its peer, its queued payloads (and the new ports in them) and its listeners alive. So an unreachable channel (a cycle) is pruned,
//! with its queued messages, at the next `collect`. A parked port (`park_port`) lives only between the walk and `make_ports` of one call. Ids of ports are generation checked, so a reused slot never
//! finds the state of an old port. Events are pruned by `collect` only: collect between pumps in a long run.

use crate::browser::*;
use crate::browser_navigation_state::Transfer;
use crate::browser_port_hub::Wire;
use crate::browser_scoped::*;
use bare_html::dom::NodeId;
use bare_js::eval::{Error, Interp, WallClock};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Attrs, HostFn, Local, Type};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, VecDeque};

type V = R<Value>;

/// Bounds of the scheduler. Each one is checked before the thing grows and ends with `Limit(Host(name))`.
#[derive(Clone, Copy, Debug)]
pub struct SchedLimits {
    /// Live timers (`timers`).
    pub max_timers: usize,
    /// Extra arguments of one timer (`timer-args`).
    pub max_timer_args: usize,
    /// Live message ports (`message-ports`).
    pub max_ports: usize,
    /// Queued messages in all ports (`messages`).
    pub max_queued_messages: usize,
    /// Bytes of the queued messages (`message-bytes`).
    pub max_queued_message_bytes: usize,
    /// Queued `select` event tasks of text controls (`select-tasks`).
    pub max_select_tasks: usize,
    /// Queued `selectionchange` tasks of text controls, at most one per control (`selectionchange-tasks`).
    pub max_selectionchange_tasks: usize,
    /// Queued events and steps of media elements (`media-tasks`).
    pub max_media_tasks: usize,
}

impl Default for SchedLimits {
    fn default() -> Self {
        SchedLimits { max_timers: 1 << 16, max_timer_args: 256, max_ports: 1 << 14, max_queued_messages: 1 << 16, max_queued_message_bytes: 16 << 20, max_select_tasks: 1 << 14, max_selectionchange_tasks: 1 << 14, max_media_tasks: 1 << 14 }
    }
}

/// A timer is recurring only if its delay is at least one frame. A faster one is a work loop, not a clock or a poll.
const RECURRING_MIN_MS: u32 = 16;
/// A run is quiet if it changes at most this many nodes, attributes or texts of the DOM.
const QUIET_CHANGES: u64 = 8;

/// What a run of a timer or a frame can leave behind: counters read before and after it.
struct Mark {
    changes: u64,
    queued: usize,
    seq: u64,
}

/// `(due time as f64 bits, sequence)`. The bits of a non-negative f64 sort like the number.
pub(crate) type Key = (u64, u64);

#[derive(Debug)]
pub(crate) enum Task {
    Timer(u64),
    Deliver(ObjId),
    /// `select` event of a text control (`browser_selection.rs`).
    Select(NodeId),
    /// `selectionchange` event of a text control (`browser_selection.rs`).
    SelectionChange(NodeId),
    /// `selectionchange` event at the document, for the Range and Selection API (`browser_range.rs`). At most one is queued.
    DocSelectionChange,
    /// The rendering step: `ResizeObserver` delivery (`browser_resize.rs`). At most one is queued.
    Render,
    /// The `scroll` events of a change of an offset (`browser_scroll.rs`). At most one is queued.
    Scroll,
    /// `AbortSignal.timeout` (`browser_abort.rs`). The task roots its signal.
    AbortTimeout(ObjId),
    /// The `error` event of a `Worker` that did not start (`browser_worker.rs`). The task roots its worker.
    Worker(ObjId),
    /// The delivery of `IntersectionObserver` entries (`browser_intersection.rs`). At most one is queued.
    Notify,
    /// An idle period (`browser_idle.rs`). At most one is queued.
    Idle,
    /// The timeout of a `requestIdleCallback` callback (`browser_idle.rs`).
    IdleTimeout(u32),
    /// An event or a step of a media element (`browser_media.rs`): the node, the epoch of its load algorithm, the step.
    Media(NodeId, u32, u8),
    /// A step of a CSS transition or animation: its number and the step (`browser_motion.rs`).
    Motion(u64, u8),
    /// The `toggle` event of a `details` (`browser_details.rs`): the node, the old state, the new state (`true` is open). At most one for each node.
    Toggle(NodeId, bool, bool),
}

/// The callback of a timer and its arguments.
struct TimerCall {
    cb: bare_js_api::Persistent,
    args: Vec<TimerArg>,
}

/// An argument of a timer. A value with no reference into the heap needs no root, so it stays as it is.
enum TimerArg {
    Plain(Value),
    Held(bare_js_api::Persistent),
}

impl TimerArg {
    fn new(s: &mut Sc<'_, '_>, l: Local<'_>) -> Self {
        match s.type_of(l) {
            Type::Undefined | Type::Null | Type::Boolean | Type::Number | Type::BigInt => TimerArg::Plain(bare_js_api::Engine::raw(s.engine(), l)),
            _ => TimerArg::Held(s.persist(l)),
        }
    }

    fn value(&self, it: &Interp<'_>) -> Value {
        match self {
            TimerArg::Plain(v) => *v,
            TimerArg::Held(p) => it.persistent_value(p),
        }
    }
}

struct Timer {
    call: TimerCall,
    /// `Some(ms)` for `setInterval`.
    interval: Option<u32>,
    nesting: u32,
    /// The queued task, or `None` while the callback of an interval runs.
    key: Option<Key>,
    /// Recurring (see "Settled" below): the delay in ms of an interval, or of a timeout that its own callback armed. 0: a one-shot.
    period: u32,
    /// A recurring timer whose last run was quiet. The settle rule skips it.
    idle: bool,
}

struct Port {
    peer: Option<ObjId>,
    enabled: bool,
    /// Queued messages.
    inbox: VecDeque<Msg>,
    /// The queued delivery tasks of this port, oldest first: one per message once the port is enabled, else none.
    keys: VecDeque<Key>,
    /// Listener id of the `onmessage` handler.
    handler: Option<u64>,
    /// The port was transferred. The object stays, with no peer and no queue. It cannot be transferred again.
    moved: bool,
    /// Its end in the port hub, once the port crossed to another page or its peer did (`browser_port_hub.rs`).
    me: Option<u64>,
    /// The end of its peer, when the peer is in another page. A port has `peer` or `remote`, never both.
    remote: Option<u64>,
}

/// A queued message: the clone, its byte cost and the new ports of its transfer list (`MessageEvent.ports`).
struct Msg {
    data: Value,
    bytes: usize,
    ports: Box<[ObjId]>,
    /// A message from another page (`browser_child.rs`); `None`: a message of this page.
    from: Option<Box<Sender>>,
}

/// The sender of a message from another page: its origin, and its window in this heap (`None`: no window for it, as for a grandchild).
pub(crate) struct Sender {
    pub origin: String,
    pub source: Option<ObjId>,
}

impl Port {
    fn new(peer: Option<ObjId>, enabled: bool) -> Port {
        Port { peer, enabled, inbox: VecDeque::new(), keys: VecDeque::new(), handler: None, moved: false, me: None, remote: None }
    }
}

struct MsgEvent {
    data: Value,
    /// The new ports of the transfer list.
    moved: Box<[ObjId]>,
    /// The frozen `ports` array, made on the first read.
    ports: Option<Value>,
    /// A message of `window.postMessage` (`browser_basics.rs`): `origin` is the document origin and `source` is the window.
    win: bool,
    /// A message from another page: its `origin` and `source`. Else `win` decides.
    from: Option<Box<Sender>>,
}

#[derive(Clone, Copy)]
struct SProtos {
    port: ObjId,
    channel: ObjId,
    event: ObjId,
    perf: ObjId,
}

#[derive(Default)]
pub(crate) struct Sched {
    limits: SchedLimits,
    /// `Some(t)`: the virtual clock reads `t`. `None`: the monotonic clock.
    virtual_now: Option<f64>,
    /// The last reading of the monotonic clock (ms). The page has seen no time after it.
    last_read: Cell<f64>,
    protos: Option<SProtos>,
    tasks: BTreeMap<Key, Task>,
    seq: u64,
    /// Tasks queued, but not the rendering step or a message: a timer that asks for a frame or posts a message has not queued work (the
    /// frame and the message are judged on their own, `follow_up`).
    work: u64,
    next_timer: u64,
    timers: HashMap<u64, Timer>,
    ports: IdMap<ObjId, Port>,
    /// Ports taken out of `ports` by a transfer, until `make_ports` gives them a new object. The key is heap-free.
    parked: HashMap<u64, (ObjId, Port)>,
    next_park: u64,
    /// The ports of this page that have an end in the hub, by end id (in id order, so a take is deterministic).
    ends: BTreeMap<u64, ObjId>,
    msg_events: IdMap<ObjId, MsgEvent>,
    queued: usize,
    queued_bytes: usize,
    /// Queued `Task::Select`.
    selects: usize,
    /// Queued `Task::SelectionChange`.
    changes: usize,
    /// Queued `Task::Media`.
    media: usize,
    /// The queued `Task::Toggle` of each `details`.
    pub(crate) toggles: HashMap<NodeId, (Key, bool)>,
    /// Nesting level of the timer task that runs now (0 outside a timer task).
    nesting: u32,
    halted: Option<Error>,
    /// The callback of the timer task that runs now. A `setTimeout` with the same function is a re-arm.
    running: Option<ObjId>,
    /// The timer that the running callback armed again.
    rearmed: Option<u64>,
    /// The rendering step runs now (a `requestAnimationFrame` call inside it is a re-registration).
    in_frame: bool,
    /// The last rendering step was quiet and no outside call asked for a frame since.
    frame_idle: bool,
    /// The last idle period was quiet and no outside request since (`browser_idle.rs`).
    idle_quiet: bool,
    /// An idle callback runs now (a request inside it is the chain asking again).
    pub(crate) in_idle: bool,
}

impl Sched {
    /// Does the settle rule skip this task? A recurring timer whose last run was quiet, and a rendering step after a quiet one.
    fn skips(&self, t: &Task) -> bool {
        match t {
            Task::Timer(id) => self.timers.get(id).is_some_and(|t| t.idle),
            Task::Render => self.frame_idle,
            Task::Motion(_, step) => *step == crate::browser_motion::ITER_INFINITE,
            Task::Idle | Task::IdleTimeout(_) => self.idle_quiet,
            _ => false,
        }
    }
}

/// What a pump did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pump {
    /// Tasks that ran.
    pub ran: usize,
    /// True when it stopped at `max_tasks` with more work due.
    pub capped: bool,
    /// Due time (ms) of the earliest queued task, if any.
    pub next_due: Option<f64>,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SchedStats {
    pub timers: usize,
    pub tasks: usize,
    pub ports: usize,
    pub queued_messages: usize,
    pub queued_message_bytes: usize,
    pub message_events: usize,
    /// The pump ended with a non-throw error and runs no more JS.
    pub halted: bool,
}

/// A task taken out of the queue, ready to run without any borrow.
enum Run {
    /// `call` is taken from a timeout; an interval keeps it in its `Timer`.
    Timer { id: u64, call: Option<TimerCall>, nesting: u32, repeat: bool },
    Deliver { port: ObjId, data: Value, ports: Box<[ObjId]>, from: Option<Box<Sender>> },
    Select(NodeId),
    SelectionChange(NodeId),
    DocSelectionChange,
    Render,
    Scroll,
    AbortTimeout(ObjId),
    Worker(ObjId),
    Notify,
    Idle,
    IdleTimeout(u32),
    Media(NodeId, u32, u8),
    Motion(u64, u8),
    Toggle(NodeId, bool, bool),
    Nothing,
}

fn to_int32(n: f64) -> i32 {
    if !n.is_finite() { 0 } else { n.trunc().rem_euclid(4294967296.0) as u32 as i32 }
}

impl Dom {
    /// `Browser::survive`: the pump stopped on a script budget; let it run again.
    pub(crate) fn clear_budget_halt_sched(&self) {
        let mut s = self.sched.borrow_mut();
        if s.halted.as_ref().is_some_and(crate::browser::is_budget) {
            s.halted = None;
        }
    }

    /// Milliseconds since the time origin, on the clock in use.
    pub(crate) fn now_ms(&self) -> f64 {
        if bare_net::vclock::on() {
            return bare_net::vclock::now() - self.vorigin;
        }
        let s = self.sched.borrow();
        match s.virtual_now {
            Some(t) => t,
            None => {
                let t = self.start.elapsed().as_secs_f64() * 1000.0;
                s.last_read.set(t);
                t
            }
        }
    }

    /// The time `t` of this document on the benchmark clock. The first value `g` with `g - vorigin >= t`: a sum and a difference of doubles can
    /// give a value just below `t`, and then a task that is due at `t` would not run at the time that the clock moved to for it.
    pub(crate) fn global(&self, t: f64) -> f64 {
        let mut g = t + self.vorigin;
        while g - self.vorigin < t {
            g = g.next_up();
        }
        g
    }

    /// Virtual clock only: move the clock to `t` ms since the time origin of this document (it never goes back).
    fn move_clock(&self, t: f64) {
        if bare_net::vclock::on() {
            bare_net::vclock::advance_to(self.global(t));
        } else if let Some(c) = &mut self.sched.borrow_mut().virtual_now {
            *c = c.max(t);
        }
    }

    /// The roots of the scheduler: prototypes and the ports with queued tasks. Timers hold persistent handles, which the engine roots.
    pub(crate) fn sched_roots(&self, out: &mut Vec<Value>) {
        let s = self.sched.borrow();
        if let Some(p) = s.protos {
            out.extend([p.port, p.channel, p.event, p.perf].map(Value::Object));
        }
        // A port with a peer in another page that listens is a root: a message can come at any time.
        out.extend(s.ports.iter().filter(|(_, p)| !p.keys.is_empty() || p.remote.is_some() && p.enabled).map(|(&o, _)| Value::Object(o)));
        out.extend(s.tasks.values().filter_map(|t| if let Task::AbortTimeout(o) | Task::Worker(o) = t { Some(Value::Object(*o)) } else { None }));
    }

    pub(crate) fn sched_trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        let s = self.sched.borrow();
        if let Some(p) = s.ports.get(&owner) {
            out.extend(p.peer.map(Value::Object));
            for m in &p.inbox {
                out.push(m.data);
                out.extend(m.ports.iter().map(|&o| Value::Object(o)));
                out.extend(m.from.iter().filter_map(|f| f.source).map(Value::Object));
            }
        } else if let Some(e) = s.msg_events.get(&owner) {
            out.push(e.data);
            out.extend(e.moved.iter().map(|&o| Value::Object(o)));
            out.extend(e.ports);
            out.extend(e.from.iter().filter_map(|f| f.source).map(Value::Object));
        }
    }

    pub(crate) fn sched_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut s = self.sched.borrow_mut();
        let s = &mut *s;
        if !s.ends.is_empty() {
            let hub = self.kids.hub();
            s.ends.retain(|&e, &mut o| {
                let keep = live(o);
                if !keep && let Some(h) = &hub {
                    h.remove_end(e);
                }
                keep
            });
        }
        s.ports.retain(|&o, _| live(o));
        s.msg_events.retain(|&o, _| live(o));
        let ports = &s.ports;
        s.tasks.retain(|_, t| match t {
            Task::Deliver(o) => ports.contains_key(o),
            Task::Timer(_) | Task::Select(_) | Task::SelectionChange(_) | Task::DocSelectionChange | Task::Render | Task::Scroll | Task::AbortTimeout(_) | Task::Worker(_) | Task::Notify | Task::Idle | Task::IdleTimeout(_) | Task::Media(..) | Task::Motion(..) | Task::Toggle(..) => true,
        });
        s.queued = s.ports.values().map(|p| p.inbox.len()).sum();
        s.queued_bytes = s.ports.values().flat_map(|p| p.inbox.iter().map(|m| m.bytes)).sum();
    }

    // ------------------------------------------------------------ settle

    fn settle_mark(&self) -> Mark {
        let d = self.doc.borrow();
        let queued = self.fetch_queued() + self.scripts_queued() + self.nav.s.borrow().tasks.len();
        Mark { changes: d.revision() + d.text_revision(), queued, seq: self.sched.borrow().work }
    }

    /// Was the run since `mark` quiet? It changed the DOM at most `QUIET_CHANGES` times, queued no fetch, script or
    /// traversal, and queued no task but `own` (the re-arm of itself).
    fn quiet_since(&self, m: Mark, own: u64) -> bool {
        let n = self.settle_mark();
        n.changes - m.changes <= QUIET_CHANGES && n.queued <= m.queued && n.seq - m.seq <= own
    }

    /// Was the frame or the message that a timer set off quiet? A noisy one makes the timers be judged again: the tick that asked for
    /// it was judged without it (`Sched::work`).
    fn follow_up(&self, m: Mark) -> bool {
        let quiet = self.quiet_since(m, 0);
        if !quiet {
            self.sched.borrow_mut().timers.values_mut().for_each(|t| t.idle = false);
        }
        quiet
    }

    /// The driver did work that no run of a timer or a frame saw (it supplied a style sheet): no verdict stands. Each recurring
    /// timer and the frame loop are judged again by their next run.
    pub(crate) fn forget_quiet(&self) {
        let mut s = self.sched.borrow_mut();
        s.frame_idle = false;
        s.idle_quiet = false;
        s.timers.values_mut().for_each(|t| t.idle = false);
    }

    /// An idle callback was requested: from outside the idle chain the verdict is taken back.
    pub(crate) fn idle_asked(&self) {
        let mut s = self.sched.borrow_mut();
        if !s.in_idle {
            s.idle_quiet = false;
        }
    }

    // ------------------------------------------------------------ queue

    /// Queue the rendering step at the next frame of the grid (`browser_frame.rs`) unless one is queued.
    pub(crate) fn want_frame(&self) -> R<()> {
        {
            // A frame that someone outside the rendering step asked for was not judged.
            let mut s = self.sched.borrow_mut();
            if !s.in_frame {
                s.frame_idle = false;
            }
        }
        if !self.frame.queued.get() {
            let f = crate::browser_frame::FRAME_MS;
            let now = self.now_ms();
            let mut due = ((now / f).floor() + 1.0) * f;
            if due - now < 1e-6 {
                due += f;
            }
            self.schedule(due, Task::Render)?;
            self.frame.queued.set(true);
            self.frame.due.set(due);
        }
        Ok(())
    }

    /// Queue the delivery of `IntersectionObserver` entries now (a task after the current one).
    pub(crate) fn want_notify(&self) -> R<()> {
        self.schedule(self.now_ms(), Task::Notify).map(|_| ())
    }

    pub(crate) fn schedule(&self, due: f64, task: Task) -> R<Key> {
        let mut s = self.sched.borrow_mut();
        let seq = s.seq;
        s.seq = seq.checked_add(1).ok_or_else(|| self.lim("task-seq"))?;
        s.work += !matches!(task, Task::Render | Task::Deliver(_) | Task::Idle | Task::IdleTimeout(_)) as u64;
        let key = (due.to_bits(), seq);
        s.tasks.insert(key, task);
        Ok(key)
    }

    /// Check the caps of the tasks that a selection change would queue: `select` (always one per change) and
    /// `selectionchange` (none when this input has one pending). Nothing changes. Call it before any state write.
    pub(crate) fn selection_room(&self, n: NodeId, change: bool, select: bool) -> R<()> {
        let s = self.sched.borrow();
        if select && s.selects >= s.limits.max_select_tasks {
            return Err(self.lim("select-tasks"));
        }
        if change && s.changes >= s.limits.max_selectionchange_tasks && !self.st.borrow().inputs.get(&n).is_some_and(|i| i.sc_pending) {
            return Err(self.lim("selectionchange-tasks"));
        }
        Ok(())
    }

    /// Queue what a selection change needs, after `selection_room` said yes. Selection API "schedule a selectionchange event": nothing
    /// when this input has a task pending; else set the flag and queue. Chrome 155 (`tools/fixtures/ui/chrome-sel.txt`): a change by script
    /// queues `selectionchange` now and `select` for the next frame, where it runs before the callbacks of `requestAnimationFrame` (Chrome
    /// puts the event in the queue of the frame). A change by the user (`user`) queues `select` now, before `selectionchange`.
    pub(crate) fn queue_selection_tasks(&self, n: NodeId, change: bool, select: bool, user: bool) -> R<()> {
        // A key's `select` goes before `selectionchange`; the one of a press or a drag in a text field goes after it (Chrome).
        let after = user && self.st.borrow().ui.sel_drag.is_some();
        if select && user && !after {
            self.schedule(self.now_ms(), Task::Select(n))?;
            self.sched.borrow_mut().selects += 1;
        }
        if change && !self.st.borrow().inputs.get(&n).is_some_and(|i| i.sc_pending) {
            self.schedule(self.now_ms(), Task::SelectionChange(n))?;
            self.sched.borrow_mut().changes += 1;
            self.st.borrow_mut().inputs.entry(n).or_default().sc_pending = true;
        }
        if select && after {
            self.schedule(self.now_ms(), Task::Select(n))?;
            self.sched.borrow_mut().selects += 1;
        }
        if select && !user {
            self.want_frame()?;
            // Just before the frame of the grid, so the task runs before the rendering step even if that was queued earlier.
            self.schedule(self.frame.due.get() - 1e-6, Task::Select(n))?;
            self.sched.borrow_mut().selects += 1;
        }
        Ok(())
    }

    /// Queue a step of the media element `n` (`browser_media.rs`), `delay` ms after the tasks that are due now. Counts against `max_media_tasks`.
    pub(crate) fn queue_media(&self, n: NodeId, epoch: u32, step: u8, delay: f64) -> R<()> {
        {
            let s = self.sched.borrow();
            if s.media >= s.limits.max_media_tasks {
                return Err(self.lim("media-tasks"));
            }
        }
        self.schedule(self.now_ms() + delay, Task::Media(n, epoch, step))?;
        self.sched.borrow_mut().media += 1;
        Ok(())
    }

    /// Queue the abort of a `AbortSignal.timeout` signal in `ms` ms. It counts against `max_timers`.
    pub(crate) fn queue_abort_timeout(&self, o: ObjId, ms: f64) -> R<()> {
        let n = self.sched.borrow().tasks.values().filter(|t| matches!(t, Task::AbortTimeout(_))).count();
        if n + self.sched.borrow().timers.len() >= self.sched.borrow().limits.max_timers {
            return Err(self.lim("timers"));
        }
        self.schedule(self.now_ms() + ms, Task::AbortTimeout(o)).map(|_| ())
    }

    pub(crate) fn unschedule(&self, key: Key) {
        self.sched.borrow_mut().tasks.remove(&key);
    }

    pub(crate) fn next_due(&self) -> Option<f64> {
        self.sched.borrow().tasks.keys().next().map(|k| f64::from_bits(k.0))
    }

    /// Take the earliest task if it is due at `upto` or before. Gives its due time.
    fn pop_due(&self, upto: f64) -> Option<(f64, Run)> {
        let mut s = self.sched.borrow_mut();
        let s = &mut *s;
        let first = s.tasks.first_entry()?;
        let due = f64::from_bits(first.key().0);
        if due > upto {
            return None;
        }
        let key = *first.key();
        let task = first.remove();
        let run = match task {
            Task::Timer(id) => match s.timers.get_mut(&id) {
                None => Run::Nothing,
                Some(t) => {
                    let (nesting, repeat) = (t.nesting, t.interval.is_some());
                    let call = if repeat {
                        t.key = None;
                        None
                    } else {
                        s.timers.remove(&id).map(|t| t.call)
                    };
                    Run::Timer { id, call, nesting, repeat }
                }
            },
            Task::Select(n) => {
                s.selects -= 1;
                Run::Select(n)
            }
            Task::SelectionChange(n) => {
                s.changes -= 1;
                // "Firing selectionchange event" step 1: the flag is cleared before anything is dispatched.
                if let Some(i) = self.st.borrow_mut().inputs.get_mut(&n) {
                    i.sc_pending = false;
                }
                Run::SelectionChange(n)
            }
            Task::DocSelectionChange => {
                self.range_task_done();
                Run::DocSelectionChange
            }
            Task::Media(n, epoch, step) => {
                s.media -= 1;
                Run::Media(n, epoch, step)
            }
            Task::Render => {
                self.frame.queued.set(false);
                Run::Render
            }
            Task::Scroll => Run::Scroll,
            Task::Motion(id, step) => Run::Motion(id, step),
            Task::Toggle(n, old, new) => {
                s.toggles.remove(&n);
                Run::Toggle(n, old, new)
            }
            Task::Notify => Run::Notify,
            Task::Idle => Run::Idle,
            Task::IdleTimeout(id) => Run::IdleTimeout(id),
            Task::AbortTimeout(o) => Run::AbortTimeout(o),
            Task::Worker(o) => Run::Worker(o),
            Task::Deliver(port) => match s.ports.get_mut(&port) {
                Some(p) if p.keys.front() == Some(&key) => {
                    p.keys.pop_front();
                    match p.inbox.pop_front() {
                        Some(Msg { data, bytes, ports, from }) => {
                            s.queued -= 1;
                            s.queued_bytes -= bytes;
                            Run::Deliver { port, data, ports, from }
                        }
                        None => Run::Nothing,
                    }
                }
                _ => Run::Nothing,
            },
        };
        Some((due, run))
    }

    /// Run one task: one `host_scope`, then a checkpoint. A throw of the callback is reported.
    fn run_task(&self, it: &mut Interp<'_>, run: Run) -> R<()> {
        match run {
            Run::Nothing => Ok(()),
            Run::Timer { id, call, nesting, repeat } => {
                let values = |c: &TimerCall| (it.persistent_value(&c.cb), c.args.iter().map(|a| a.value(it)).collect::<Vec<Value>>());
                let (cb, args) = match &call {
                    Some(c) => values(c),
                    None => match self.sched.borrow().timers.get(&id) {
                        Some(t) => values(&t.call),
                        None => return Ok(()),
                    },
                };
                drop(call);
                let mark = self.settle_mark();
                {
                    let mut s = self.sched.borrow_mut();
                    s.nesting = nesting;
                    s.running = if let Value::Object(o) = cb { Some(o) } else { None };
                    s.rearmed = None;
                }
                let this = Value::Object(*self.global.get().expect("installed"));
                let r = it.host_scope(|it| {
                    it.charge(1)?;
                    match it.call(cb, this, &args) {
                        Ok(_) => {}
                        Err(Error::Throw(v)) => self.report(it, v, if repeat { "interval" } else { "timeout" }, Target::Window),
                        Err(e) => return Err(e),
                    }
                    it.host_checkpoint().map(|_| ())
                });
                let rearmed = {
                    let mut s = self.sched.borrow_mut();
                    s.nesting = 0;
                    s.running = None;
                    s.rearmed.take()
                };
                r?;
                let quiet = self.quiet_since(mark, rearmed.is_some() as u64);
                let mut s = self.sched.borrow_mut();
                for i in [repeat.then_some(id), rearmed].into_iter().flatten() {
                    if let Some(t) = s.timers.get_mut(&i) {
                        t.idle = t.period > 0 && quiet;
                    }
                }
                drop(s);
                if repeat {
                    self.requeue_interval(id, nesting + 1)?;
                }
                Ok(())
            }
            Run::Select(n) | Run::SelectionChange(n) => it.host_scope(|it| {
                it.charge(1)?;
                self.fire_at_input(it, n, if matches!(run, Run::Select(_)) { "select" } else { "selectionchange" })?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::DocSelectionChange => it.host_scope(|it| {
                it.charge(1)?;
                self.fire_at_document(it, "selectionchange")?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::Toggle(n, old, new) => it.host_scope(|it| {
                it.charge(1)?;
                self.toggle_task(it, n, old, new)?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::Media(n, epoch, step) => it.host_scope(|it| {
                it.charge(1)?;
                self.media_task(it, n, epoch, step)?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::Render => {
                let mark = self.settle_mark();
                self.sched.borrow_mut().in_frame = true;
                let r = it.host_scope(|it| {
                    it.charge(1)?;
                    self.frame_run(it)
                });
                self.sched.borrow_mut().in_frame = false;
                r?;
                // The step queues the next one itself while callbacks wait. That is not counted as work (see `Sched::work`).
                let quiet = self.follow_up(mark);
                self.sched.borrow_mut().frame_idle = quiet;
                Ok(())
            }
            Run::Motion(id, step) => {
                it.host_scope(|it| {
                    it.charge(1)?;
                    self.motion_task(it, id, step)?;
                    it.host_checkpoint().map(|_| ())
                })
            }
            Run::Idle | Run::IdleTimeout(_) => {
                let mark = self.settle_mark();
                self.sched.borrow_mut().in_idle = true;
                let r = it.host_scope(|it| {
                    it.charge(1)?;
                    match run {
                        Run::IdleTimeout(id) => self.idle_timeout(it, id),
                        _ => self.idle_run(it),
                    }
                });
                self.sched.borrow_mut().in_idle = false;
                // A period that only moved behind a task or the frame ran nothing and has no verdict.
                if r? {
                    let quiet = self.follow_up(mark);
                    self.sched.borrow_mut().idle_quiet = quiet;
                }
                Ok(())
            }
            Run::Notify => it.host_scope(|it| {
                it.charge(1)?;
                self.io_notify(it)
            }),
            Run::Scroll => it.host_scope(|it| {
                it.charge(1)?;
                self.flush_scroll(it)?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::AbortTimeout(o) => it.host_scope(|it| {
                it.charge(1)?;
                self.abort_timeout_fired(it, o)?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::Worker(o) => it.host_scope(|it| {
                it.charge(1)?;
                self.worker_failed(it, o)?;
                it.host_checkpoint().map(|_| ())
            }),
            Run::Deliver { port, data, ports, from } => {
                let mark = self.settle_mark();
                it.host_scope(|it| {
                    it.charge(1)?;
                    let win = self.global.get() == Some(&port);
                    let target = if win { Target::Window } else { Target::Obj(port) };
                    if self.has_message_listener(target) {
                        let ev = self.new_msg_event(it, data, win, ports, from)?;
                        self.dispatch(it, target, ev)?;
                    }
                    it.host_checkpoint().map(|_| ())
                })?;
                self.follow_up(mark);
                Ok(())
            }
        }
    }

    fn has_message_listener(&self, target: Target) -> bool {
        self.st.borrow().listeners.get(&target).is_some_and(|v| v.iter().any(|l| &*l.ty == "message"))
    }

    fn requeue_interval(&self, id: u64, nesting: u32) -> R<()> {
        let now = self.now_ms();
        let Some((interval, level)) = self.sched.borrow().timers.get(&id).map(|t| (t.interval.unwrap_or(0), nesting.min(1 << 20))) else { return Ok(()) };
        let delay = if level > 5 { interval.max(4) } else { interval };
        let key = self.schedule(now + delay as f64, Task::Timer(id))?;
        let mut s = self.sched.borrow_mut();
        if let Some(t) = s.timers.get_mut(&id) {
            t.key = Some(key);
            t.nesting = level;
        }
        Ok(())
    }

    // ------------------------------------------------------------ install

    pub(crate) fn install_scheduler(&self, it: &mut Interp<'_>) -> R<()> {
        let g = it.global_object();
        let p = self.p();
        let op = it.object_prototype();
        let (port, channel, event, perf) = (self.obj(it, p.et)?, self.obj(it, op)?, self.obj(it, p.event)?, self.obj(it, op)?);
        self.sched.borrow_mut().protos = Some(SProtos { port, channel, event, perf });
        let kc = self.key(it, "constructor")?;
        let kp = self.key(it, "prototype")?;
        for (name, h, len) in [
            ("setTimeout", set_timeout as HS, 1),
            ("setInterval", set_interval as HS, 1),
            ("clearTimeout", clear_timer as HS, 1),
            ("clearInterval", clear_timer as HS, 1),
            ("queueMicrotask", queue_microtask as HS, 1),
        ] {
            let f = self.add_fn_s_obj(it, h, name, len, HostFn::Method)?;
            let k = self.key(it, name)?;
            it.define_data(g, k, Value::Object(f), true, true, true)?;
        }
        // interface objects
        for (name, proto, ctor) in [
            ("MessageChannel", channel, Some(message_channel_ctor as HS)),
            ("MessagePort", port, None),
            ("MessageEvent", event, None),
        ] {
            let f = self.add_fn_s_obj(it, ctor.unwrap_or(illegal), name, 0, if ctor.is_some() { HostFn::Ctor } else { HostFn::Method })?;
            it.define_data(f, kp, Value::Object(proto), false, false, false)?;
            it.define_data(proto, kc, Value::Object(f), true, false, true)?;
            let k = self.key(it, name)?;
            it.define_data(g, k, Value::Object(f), true, false, true)?;
        }
        let acc = |it: &mut Interp<'_>, on: ObjId, name: &str, get: HS, set: Option<HS>| -> R<()> {
            let key = self.key(it, name)?;
            let gf = self.add_fn_s_obj(it, get, &format!("get {name}"), 0, HostFn::Method)?;
            let sf = match set {
                Some(s) => Some(self.add_fn_s_obj(it, s, &format!("set {name}"), 1, HostFn::Method)?),
                None => None,
            };
            let d = bare_js::heap::PropertyDescriptor { get: Some(Some(gf)), set: Some(sf), enumerable: Some(true), configurable: Some(true), ..Default::default() };
            match it.heap.define_own_property(on, key, &d) {
                Ok(_) => Ok(()),
                Err(e) => Err(it.heap_error(e)),
            }
        };
        let method = |it: &mut Interp<'_>, on: ObjId, name: &str, h: HS, len: u32| -> R<()> {
            let f = self.add_fn_s_obj(it, h, name, len, HostFn::Method)?;
            let k = self.key(it, name)?;
            it.define_data(on, k, Value::Object(f), true, true, true)
        };
        method(it, port, "postMessage", port_post_message, 1)?;
        method(it, port, "start", port_start, 0)?;
        method(it, port, "close", port_close, 0)?;
        acc(it, port, "onmessage", port_onmessage, Some(set_port_onmessage))?;
        for (name, h) in [("data", msg_data as HS), ("origin", msg_origin), ("lastEventId", msg_empty), ("source", msg_source), ("ports", msg_ports)] {
            acc(it, event, name, h, None)?;
        }
        method(it, perf, "now", performance_now, 0)?;
        crate::browser_perf::install(self, it, perf)?;
        let po = self.obj(it, perf)?;
        let k = self.key(it, "performance")?;
        it.define_data(g, k, Value::Object(po), true, true, true)?;
        Ok(())
    }

    // ------------------------------------------------------------ messages

    /// Queue a `message` event for the window: one delivery task. The window is a port of the table with no peer. `data` is already cloned.
    pub(crate) fn post_window(&self, data: Value, ports: Box<[ObjId]>) -> R<()> {
        self.queue_window(data, ports, crate::browser_basics::MESSAGE_COST, None)
    }

    /// The most messages that a page queues (also the bound of the outbox for other pages).
    pub(crate) fn max_messages(&self) -> usize {
        self.sched.borrow().limits.max_queued_messages
    }

    /// `post_window` with the byte cost, and the sender when the message comes from another page.
    pub(crate) fn queue_window(&self, data: Value, ports: Box<[ObjId]>, cost: usize, from: Option<Box<Sender>>) -> R<()> {
        let g = *self.global.get().expect("installed");
        {
            let s = self.sched.borrow();
            if s.queued >= s.limits.max_queued_messages {
                return Err(self.lim("messages"));
            }
            if s.queued_bytes.saturating_add(cost) > s.limits.max_queued_message_bytes {
                return Err(self.lim("message-bytes"));
            }
        }
        let key = self.schedule(self.now_ms(), Task::Deliver(g))?;
        let mut s = self.sched.borrow_mut();
        s.queued += 1;
        s.queued_bytes += cost;
        let p = s.ports.entry(g).or_insert(Port::new(None, true));
        p.inbox.push_back(Msg { data, bytes: cost, ports, from });
        p.keys.push_back(key);
        Ok(())
    }

    fn new_msg_event(&self, it: &mut Interp<'_>, data: Value, win: bool, moved: Box<[ObjId]>, from: Option<Box<Sender>>) -> R<ObjId> {
        let proto = self.sched.borrow().protos.expect("installed").event;
        let e = self.new_event_proto(it, "message", false, false, false, proto)?;
        self.sched.borrow_mut().msg_events.insert(e, MsgEvent { data, moved, ports: None, win, from });
        Ok(e)
    }

    fn port_of<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
        match obj_id(s, this).filter(|o| self.sched.borrow().ports.contains_key(o)) {
            Some(o) => Ok(o),
            None => Err(s.type_error("Illegal invocation")),
        }
    }

    /// Queue a delivery task for each message of `port` that has none, if the port is enabled.
    fn start_port(&self, port: ObjId) -> R<()> {
        let now = self.now_ms();
        let missing = {
            let s = self.sched.borrow();
            match s.ports.get(&port) {
                Some(p) if !p.enabled => p.inbox.len(),
                _ => 0,
            }
        };
        if let Some(p) = self.sched.borrow_mut().ports.get_mut(&port) {
            p.enabled = true;
        }
        for _ in 0..missing {
            let key = self.schedule(now, Task::Deliver(port))?;
            if let Some(p) = self.sched.borrow_mut().ports.get_mut(&port) {
                p.keys.push_back(key);
            }
        }
        Ok(())
    }

    /// The message, its byte cost and the new ports of its transfer list, or the error that `postMessage` throws.
    /// No list: a scalar needs no snapshot.
    pub(crate) fn clone_payload(&self, it: &mut Interp<'_>, v: Value, list: &[ObjId]) -> R<(Value, usize, Box<[ObjId]>)> {
        if !list.is_empty() {
            let (data, ports, bytes) = crate::browser_navigation_state::clone_with(self, it, v, list)?;
            return Ok((data, 16usize.saturating_add(bytes), ports.into_boxed_slice()));
        }
        let none = Box::default();
        match v {
            Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) => Ok((v, 16, none)),
            Value::String(s) => {
                let len = it.heap.str_len(s).map_err(Error::Heap)?;
                it.charge((len / 64) as u64)?;
                Ok((v, 16usize.saturating_add(len.saturating_mul(2)), none))
            }
            Value::Symbol(_) => Err(self.dom_err(it, "DataCloneError", "Symbol could not be cloned.")),
            Value::BigInt(_) | Value::BigHeap(_) => Ok((v, 16usize.saturating_add(it.heap.bigint_limbs(v).saturating_mul(8)), none)),
            Value::Object(_) if it.is_callable_value(v) => Err(self.dom_err(it, "DataCloneError", "function could not be cloned.")),
            Value::Object(_) => {
                let (data, _, bytes) = crate::browser_navigation_state::clone_with(self, it, v, &[])?;
                Ok((data, 16usize.saturating_add(bytes), none))
            }
        }
    }

    /// The transfer argument of `MessagePort.postMessage`: a sequence, or a `{ transfer }` dictionary.
    fn transfer_arg(&self, it: &mut Interp<'_>, v: Value, own: Option<ObjId>) -> R<Vec<ObjId>> {
        const CTX: &str = "Failed to execute 'postMessage' on 'MessagePort'";
        match v {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Object(_) => match it.host_iter_open(v)? {
                Some(iter) => self.transfer_items(it, iter, own),
                None => {
                    let kt = self.key(it, "transfer")?;
                    let t = it.get_value(v, kt)?;
                    self.transfer_list(it, t, own, CTX)
                }
            },
            _ => Err(it.type_error(&format!("{CTX}: The transfer argument is not an object."))),
        }
    }

    /// The items of a transfer list (HTML "StructuredSerializeWithTransfer", step 1). `v` is `undefined`, `null` or an iterable
    /// of objects. Each item is a `MessagePort` or an `ArrayBuffer`, once, and not detached or transferred before; `own` (the port
    /// of `postMessage`) is not allowed. Anything else throws `DataCloneError`. Nothing changes.
    pub(crate) fn transfer_list(&self, it: &mut Interp<'_>, v: Value, own: Option<ObjId>, ctx: &str) -> R<Vec<ObjId>> {
        if matches!(v, Value::Undefined | Value::Null) {
            return Ok(Vec::new());
        }
        let Some(iter) = it.host_iter_open(v)? else {
            return Err(it.type_error(&format!("{ctx}: The provided value cannot be converted to a sequence.")));
        };
        self.transfer_items(it, iter, own)
    }

    fn transfer_items(&self, it: &mut Interp<'_>, mut iter: bare_js::eval::HostIter, own: Option<ObjId>) -> R<Vec<ObjId>> {
        let mut list: Vec<ObjId> = Vec::new();
        while let Some(v) = it.host_iter_next(&mut iter)? {
            let bad = |d: &Dom, it: &mut Interp<'_>, why: &str| d.dom_err(it, "DataCloneError", why);
            let Value::Object(o) = v else { return Err(bad(self, it, "Value not transferable.")) };
            let buffer = matches!(it.clone_class(o), bare_js::eval::CloneClass::Buffer);
            if !buffer && !self.sched.borrow().ports.contains_key(&o) || self.global.get() == Some(&o) {
                return Err(bad(self, it, "Value not transferable."));
            }
            if Some(o) == own {
                return Err(bad(self, it, "Transfer list contains source port."));
            }
            if buffer && it.heap.buffer_detached(o) || !buffer && !self.port_movable(o) {
                return Err(bad(self, it, "Transfer list contains a detached item."));
            }
            if list.contains(&o) {
                return Err(bad(self, it, "Transfer list contains duplicate entries."));
            }
            list.push(o);
        }
        Ok(list)
    }

    /// Is `o` a port that no transfer has moved?
    pub(crate) fn port_movable(&self, o: ObjId) -> bool {
        self.global.get() != Some(&o) && self.sched.borrow().ports.get(&o).is_some_and(|p| !p.moved)
    }

    /// Take the port `o` out of the port table for a transfer. The object stays as a neutered shell. Its queued delivery tasks are
    /// dropped (the new port starts disabled, as in the Standard, and `start_port` queues them again). Returns the key for `make_ports`.
    pub(crate) fn park_port(&self, o: ObjId) -> u64 {
        let mut s = self.sched.borrow_mut();
        let s = &mut *s;
        let mut shell = Port::new(None, false);
        shell.moved = true;
        let mut rec = std::mem::replace(s.ports.get_mut(&o).expect("checked"), shell);
        for k in rec.keys.drain(..) {
            s.tasks.remove(&k);
        }
        rec.enabled = false;
        rec.handler = None;
        s.next_park += 1;
        let key = s.next_park;
        s.parked.insert(key, (o, rec));
        key
    }

    /// New port objects for parked records, in the order of `keys`. Each takes the peer link, the queue and the byte cost of its
    /// record. The peer, if it is not in `keys` too, points to the new object.
    pub(crate) fn make_ports(&self, it: &mut Interp<'_>, keys: &[u64]) -> R<Vec<ObjId>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        if self.sched.borrow().ports.len() + keys.len() > self.sched.borrow().limits.max_ports {
            return Err(self.lim("message-ports"));
        }
        if self.st.borrow().by_obj.len() + keys.len() > self.lim.max_wrappers {
            return Err(self.lim("dom-wrappers"));
        }
        let proto = self.sched.borrow().protos.expect("installed").port;
        let mut news = Vec::with_capacity(keys.len());
        for _ in keys {
            news.push(self.obj(it, proto)?);
        }
        let mut st = self.st.borrow_mut();
        let mut s = self.sched.borrow_mut();
        let olds: Vec<ObjId> = keys.iter().map(|k| s.parked[k].0).collect();
        for (k, &new) in keys.iter().zip(&news) {
            let (old, mut rec) = s.parked.remove(k).expect("parked");
            if let Some(q) = rec.peer {
                match olds.iter().position(|&o| o == q) {
                    Some(i) => rec.peer = Some(news[i]),
                    None => {
                        if let Some(r) = s.ports.get_mut(&q).filter(|r| r.peer == Some(old)) {
                            r.peer = Some(new);
                        }
                    }
                }
            }
            st.by_obj.insert(new, Target::Obj(new));
            if let Some(e) = rec.me {
                s.ends.insert(e, new);
            }
            s.ports.insert(new, rec);
        }
        Ok(news)
    }
}

fn illegal<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

// ------------------------------------------------------------------------------------------ timers

fn set_timer<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>, repeat: bool) -> Res<'s, Local<'s>> {
    crate::browser_frame::window_this(d, s, this)?;
    let name = if repeat { "setInterval" } else { "setTimeout" };
    let Some(cb) = a.get(0) else {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'Window': 1 argument required, but only 0 present.")));
    };
    if !s.is_callable(cb) {
        return Err(unsupp(match (repeat, s.type_of(cb) == Type::String) {
            (false, true) => "setTimeout with a string callback (eval)",
            (true, true) => "setInterval with a string callback (eval)",
            (false, false) => "setTimeout with a non-function callback",
            (true, false) => "setInterval with a non-function callback",
        }));
    }
    let n = match a.get(1) {
        Some(dl) => s.to_number(dl)?,
        None => f64::NAN,
    };
    let mut delay = to_int32(n).max(0) as u32;
    let argc = a.len().saturating_sub(2);
    let now = d.now_ms();
    let (id, level) = {
        let sc = d.sched.borrow();
        if argc > sc.limits.max_timer_args {
            return Err(d.lim_s(s, "timer-args"));
        }
        if sc.timers.len() >= sc.limits.max_timers {
            return Err(d.lim_s(s, "timers"));
        }
        let id = sc.next_timer + 1;
        if id > i32::MAX as u64 {
            return Err(d.lim_s(s, "timer-ids"));
        }
        (id, sc.nesting.saturating_add(1))
    };
    if level > 5 && delay < 4 {
        delay = 4;
    }
    s.charge(1 + argc as u64)?;
    let args = if argc == 0 { Vec::new() } else { (2..a.len()).map(|i| TimerArg::new(s, a.get(i).expect("in range"))).collect() };
    let call = TimerCall { cb: s.persist(cb), args };
    let key = lift(s, d.schedule(now + delay as f64, Task::Timer(id)))?;
    let cb_obj = obj_id(s, cb);
    let mut sc = d.sched.borrow_mut();
    sc.next_timer = id;
    let rearm = cb_obj.is_some_and(|o| sc.running == Some(o));
    let period = if delay >= RECURRING_MIN_MS && (repeat || rearm) { delay } else { 0 };
    if rearm && period > 0 {
        sc.rearmed = Some(id);
    }
    sc.timers.insert(id, Timer { call, interval: repeat.then_some(delay), nesting: level, key: Some(key), period, idle: false });
    Ok(s.number(id as f64))
}

fn set_timeout<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    set_timer(d, s, this, a, false)
}

fn set_interval<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    set_timer(d, s, this, a, true)
}

fn clear_timer<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    crate::browser_frame::window_this(d, s, this)?;
    let v = arg_s(s, a, 0);
    let n = s.to_number(v)?;
    let id = to_int32(n);
    let mut sc = d.sched.borrow_mut();
    if id > 0
        && let Some(t) = sc.timers.remove(&(id as u64))
        && let Some(k) = t.key
    {
        sc.tasks.remove(&k);
    }
    Ok(s.undefined())
}

fn queue_microtask<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    crate::browser_frame::window_this(d, s, this)?;
    let cb = arg_s(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'queueMicrotask' on 'Window': parameter 1 is not of type 'Function'."));
    }
    s.queue_microtask(cb)?;
    Ok(s.undefined())
}

fn performance_now<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    // On the virtual clock a read takes a little time, so that a loop that waits for `performance.now()` to pass a value ends.
    if bare_net::vclock::on() {
        bare_net::vclock::advance_by(bare_net::vclock::READ_MS);
    }
    Ok(s.number(d.now_ms()))
}

// ------------------------------------------------------------------------------------------ message ports

fn message_channel_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    if d.sched.borrow().ports.len() + 2 > d.sched.borrow().limits.max_ports {
        return Err(d.lim_s(s, "message-ports"));
    }
    if d.st.borrow().by_obj.len() + 2 > d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
    let protos = d.sched.borrow().protos.expect("installed");
    let p1 = d.obj_s(s, protos.port)?;
    let l1 = d.obj_local(s, p1);
    let p2 = d.obj_s(s, protos.port)?;
    let l2 = d.obj_local(s, p2);
    let ch = d.obj_s(s, protos.channel)?;
    let lc = d.obj_local(s, ch);
    let fixed = Attrs { writable: false, enumerable: false, configurable: false };
    s.define_str(lc, "port1", l1, fixed)?;
    s.define_str(lc, "port2", l2, fixed)?;
    let mut st = d.st.borrow_mut();
    let mut sc = d.sched.borrow_mut();
    for (me, peer) in [(p1, p2), (p2, p1)] {
        st.by_obj.insert(me, Target::Obj(me));
        sc.ports.insert(me, Port::new(Some(peer), false));
    }
    Ok(lc)
}

impl Dom {
    /// The ports of a record `c` (made by `clone_state_with`, so still `Transfer::Port`) leave for another page: each becomes a
    /// `Transfer::End`. A port with a peer in this page makes the peer a port with an end here: the pair is split. A port that crossed
    /// before keeps its ends. The messages that wait in the port move with it, as wires, in order. A waiting message that carries ports
    /// is dropped, with one warning (a known limit).
    pub(crate) fn export_ports(&self, it: &mut Interp<'_>, c: &mut bare_js_api::Cloned) -> R<()> {
        let xf = c.moved_mut();
        let port_key = |m: &bare_js_api::Moved| match m {
            bare_js_api::Moved::Host(b) => match b.downcast_ref::<Transfer>() {
                Some(Transfer::Port(k)) => Some(*k),
                _ => None,
            },
            bare_js_api::Moved::Buffer(_) => None,
        };
        if !xf.iter().any(|m| port_key(m).is_some()) {
            return Ok(());
        }
        let Some(hub) = self.kids.hub() else {
            return Err(self.dom_err(it, "DataCloneError", "A MessagePort could not move to another page."));
        };
        let page = self.kids.page.get();
        let mut recs: Vec<(usize, ObjId, Port)> = Vec::new();
        {
            let mut s = self.sched.borrow_mut();
            for (i, t) in xf.iter().enumerate() {
                if let Some(k) = port_key(t) {
                    let (o, rec) = s.parked.remove(&k).expect("parked");
                    s.queued -= rec.inbox.len();
                    s.queued_bytes -= rec.inbox.iter().map(|m| m.bytes).sum::<usize>();
                    recs.push((i, o, rec));
                }
            }
        }
        let mut warned = false;
        let mut waiting: Vec<Vec<Wire>> = Vec::with_capacity(recs.len());
        for (_, _, rec) in &mut recs {
            let mut ws = Vec::with_capacity(rec.inbox.len());
            for m in std::mem::take(&mut rec.inbox) {
                if !m.ports.is_empty() {
                    if !std::mem::replace(&mut warned, true) {
                        crate::browser_console::push_text(self, it, crate::browser_console::ConsoleLevel::Warn, "A message that waited in a MessagePort and carried ports was dropped when the port moved to another page");
                    }
                    continue;
                }
                let cloned = crate::browser_navigation_state::clone_state_with(self, it, m.data, &[])?;
                ws.push(Wire { cloned, bytes: m.bytes });
            }
            waiting.push(ws);
        }
        let olds: Vec<ObjId> = recs.iter().map(|r| r.1).collect();
        let ids: Vec<u64> = recs.iter().map(|r| r.2.me.unwrap_or_else(|| hub.new_end(None))).collect();
        for (j, (i, _, rec)) in recs.iter().enumerate() {
            let me = ids[j];
            let remote = match (rec.me, rec.peer) {
                (Some(_), _) => rec.remote,
                (None, Some(q)) => match olds.iter().position(|&o| o == q) {
                    // Both ends of the pair are in the list.
                    Some(k) => Some(ids[k]),
                    // The peer stays here: it gets an end of this page.
                    None => {
                        let mut s = self.sched.borrow_mut();
                        let s = &mut *s;
                        s.ports.get_mut(&q).map(|qp| {
                            let eq = hub.new_end(Some(page));
                            (qp.peer, qp.remote, qp.me) = (None, Some(me), Some(eq));
                            s.ends.insert(eq, q);
                            eq
                        })
                    }
                },
                (None, None) => None,
            };
            hub.set_owner(me, None);
            hub.prepend(me, std::mem::take(&mut waiting[j]));
            self.sched.borrow_mut().ends.remove(&me);
            xf[*i] = bare_js_api::Moved::Host(Box::new(Transfer::End { me, remote }));
        }
        Ok(())
    }

    /// New port objects for the ends of a message that came from another page, in the order of `ends`. Each port starts disabled and
    /// takes the wires that wait for its end. The page owns the ends from now on.
    pub(crate) fn import_ends(&self, it: &mut Interp<'_>, ends: &[(u64, Option<u64>)]) -> R<Vec<ObjId>> {
        let Some(hub) = self.kids.hub() else {
            return Err(self.dom_err(it, "DataCloneError", "A MessagePort could not move to this page."));
        };
        if self.sched.borrow().ports.len() + ends.len() > self.sched.borrow().limits.max_ports {
            return Err(self.lim("message-ports"));
        }
        if self.st.borrow().by_obj.len() + ends.len() > self.lim.max_wrappers {
            return Err(self.lim("dom-wrappers"));
        }
        let proto = self.sched.borrow().protos.expect("installed").port;
        let mut news = Vec::with_capacity(ends.len());
        for _ in ends {
            news.push(self.obj(it, proto)?);
        }
        let page = self.kids.page.get();
        {
            let mut st = self.st.borrow_mut();
            let mut s = self.sched.borrow_mut();
            for (&(me, remote), &new) in ends.iter().zip(&news) {
                let mut p = Port::new(None, false);
                // An end that was removed while the port was in flight (its message was dropped, or its page ended) leaves a closed port.
                if hub.has(me) {
                    (p.me, p.remote) = (Some(me), remote);
                    hub.set_owner(me, Some(page));
                    s.ends.insert(me, new);
                }
                st.by_obj.insert(new, Target::Obj(new));
                s.ports.insert(new, p);
            }
        }
        for (&(me, _), &new) in ends.iter().zip(&news) {
            if self.sched.borrow().ends.get(&me) == Some(&new) {
                self.take_end(it, &hub, me, new)?;
            }
        }
        Ok(news)
    }

    /// Move the wires that wait for the ends of this page into their ports (the driver calls it when the hub is busy). Returns the count.
    pub(crate) fn take_wires(&self, it: &mut Interp<'_>) -> R<usize> {
        let Some(hub) = self.kids.hub() else { return Ok(0) };
        let ends: Vec<(u64, ObjId)> = self.sched.borrow().ends.iter().map(|(&e, &o)| (e, o)).collect();
        let mut n = 0;
        for (e, o) in ends {
            n += self.take_end(it, &hub, e, o)?;
        }
        Ok(n)
    }

    /// The wires of `end` become messages of its port `obj`, in order. A message over the bounds of this page is dropped, with a warning.
    fn take_end(&self, it: &mut Interp<'_>, hub: &crate::browser_port_hub::PortHub, end: u64, obj: ObjId) -> R<usize> {
        let mut n = 0;
        while let Some(mut w) = hub.pop(end) {
            n += 1;
            let full = {
                let s = self.sched.borrow();
                s.queued >= s.limits.max_queued_messages || s.queued_bytes.saturating_add(w.bytes) > s.limits.max_queued_message_bytes
            };
            if full {
                hub.release(w.cloned.moved());
                crate::browser_console::push_text(self, it, crate::browser_console::ConsoleLevel::Warn, "A message for a MessagePort was dropped: the queue of its page is full");
                continue;
            }
            let bytes = w.bytes;
            let (data, ports) = crate::browser_navigation_state::materialize_with(self, it, &mut w.cloned)?;
            let enabled = self.sched.borrow().ports.get(&obj).is_some_and(|p| p.enabled);
            let key = if enabled { Some(self.schedule(self.now_ms(), Task::Deliver(obj))?) } else { None };
            let mut s = self.sched.borrow_mut();
            s.queued += 1;
            s.queued_bytes += bytes;
            if let Some(p) = s.ports.get_mut(&obj) {
                p.inbox.push_back(Msg { data, bytes, ports: ports.into_boxed_slice(), from: None });
                p.keys.extend(key);
            }
        }
        Ok(n)
    }

    /// A `MessagePort` with no peer (the `port` of a `SharedWorker` that did not start): it takes messages and delivers none.
    pub(crate) fn new_port(&self, it: &mut Interp<'_>) -> R<ObjId> {
        if self.sched.borrow().ports.len() + 1 > self.sched.borrow().limits.max_ports {
            return Err(self.lim("message-ports"));
        }
        if self.st.borrow().by_obj.len() + 1 > self.lim.max_wrappers {
            return Err(self.lim("dom-wrappers"));
        }
        let protos = self.sched.borrow().protos.expect("installed");
        let p = self.obj(it, protos.port)?;
        self.st.borrow_mut().by_obj.insert(p, Target::Obj(p));
        self.sched.borrow_mut().ports.insert(p, Port::new(None, false));
        Ok(p)
    }
}

fn port_post_message<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let port = d.port_of(s, this)?;
    let Some(v) = raw_arg(s, a, 0) else {
        return Err(s.type_error("Failed to execute 'postMessage' on 'MessagePort': 1 argument required, but only 0 present."));
    };
    let transfer = raw_arg(s, a, 1).unwrap_or(Value::Undefined);
    let list = old(s, |it| d.transfer_arg(it, transfer, Some(port)))?;
    let remote = d.sched.borrow().ports.get(&port).and_then(|p| p.remote);
    if let Some(end) = remote {
        old(s, |it| post_remote(d, it, v, &list, end))?;
        return Ok(s.undefined());
    }
    let (data, bytes, moved) = old(s, |it| d.clone_payload(it, v, &list))?;
    let now = d.now_ms();
    let target = {
        let sc = d.sched.borrow();
        let Some(p) = sc.ports.get(&port) else { return Ok(s.undefined()) };
        let Some(peer) = p.peer else { return Ok(s.undefined()) };
        if sc.queued >= sc.limits.max_queued_messages {
            return Err(d.lim_s(s, "messages"));
        }
        if sc.queued_bytes.saturating_add(bytes) > sc.limits.max_queued_message_bytes {
            return Err(d.lim_s(s, "message-bytes"));
        }
        peer
    };
    let enabled = d.sched.borrow().ports[&target].enabled;
    let key = if enabled { Some(lift(s, d.schedule(now, Task::Deliver(target)))?) } else { None };
    let mut sc = d.sched.borrow_mut();
    sc.queued += 1;
    sc.queued_bytes += bytes;
    let t = sc.ports.get_mut(&target).expect("checked");
    t.inbox.push_back(Msg { data, bytes, ports: moved, from: None });
    t.keys.extend(key);
    Ok(s.undefined())
}

/// `postMessage` of a port whose peer is in another page: the message goes to the hub as data. A missing end drops it.
fn post_remote(d: &Dom, it: &mut Interp<'_>, v: Value, list: &[ObjId], end: u64) -> V {
    let Some(hub) = d.kids.hub() else { return Ok(Value::Undefined) };
    if hub.wires() >= d.max_messages() {
        return Err(d.lim("messages"));
    }
    let mut cloned = crate::browser_navigation_state::clone_state_with(d, it, v, list)?;
    d.export_ports(it, &mut cloned)?;
    let bytes = 16usize.saturating_add(cloned.cost());
    if hub.bytes().saturating_add(bytes) > d.sched.borrow().limits.max_queued_message_bytes {
        return Err(d.lim("message-bytes"));
    }
    if let Err(w) = hub.push(end, Wire { cloned, bytes }) {
        hub.release(w.cloned.moved());
    }
    Ok(Value::Undefined)
}

fn port_start<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let port = d.port_of(s, this)?;
    lift(s, d.start_port(port))?;
    Ok(s.undefined())
}

fn port_close<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let port = d.port_of(s, this)?;
    let mut sc = d.sched.borrow_mut();
    let p = sc.ports.get_mut(&port).expect("checked");
    let (peer, me) = (p.peer.take(), p.me.take());
    p.remote = None;
    if let Some(q) = peer.and_then(|q| sc.ports.get_mut(&q)) {
        q.peer = None;
    }
    if let Some(e) = me {
        sc.ends.remove(&e);
        if let Some(h) = d.kids.hub() {
            h.remove_end(e);
        }
    }
    Ok(s.undefined())
}

fn port_onmessage<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let port = d.port_of(s, this)?;
    let Some(id) = d.sched.borrow().ports[&port].handler else { return Ok(s.null()) };
    let st = d.st.borrow();
    match st.listeners.get(&Target::Obj(port)).and_then(|v| v.iter().find(|l| l.id == id)) {
        Some(l) => Ok(s.traced(&l.cb)),
        None => Ok(s.null()),
    }
}

fn set_port_onmessage<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let port = d.port_of(s, this)?;
    let v = arg_s(s, a, 0);
    let cb = s.is_callable(v).then_some(v);
    let t = Target::Obj(port);
    let old = d.sched.borrow().ports[&port].handler;
    {
    let mut st = d.st.borrow_mut();
    let st = &mut *st;
    let existing = old.and_then(|id| st.listeners.get(&t).and_then(|v| v.iter().position(|l| l.id == id)));
    match (existing, cb) {
        (Some(i), Some(cb)) => {
            s.store(&mut st.listeners.get_mut(&t).expect("found")[i].cb, cb);
            d.own_listeners(st, s.engine(), t);
        }
        (Some(i), None) => {
            let v = st.listeners.get_mut(&t).expect("found");
            v.remove(i);
            if v.removable() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
            d.sched.borrow_mut().ports.get_mut(&port).expect("checked").handler = None;
        }
        (None, Some(cb)) => {
            if st.n_listeners >= d.lim.max_listeners {
                return Err(d.lim_s(s, "dom-listeners"));
            }
            let list = st.listeners.entry(t).or_default();
            if list.len() >= d.lim.max_listeners_per_target {
                return Err(d.lim_s(s, "dom-listeners-per-target"));
            }
            st.next_id += 1;
            let id = st.next_id;
            list.push(Listener { id, ty: "message".into(), cb: s.trace_ref(cb), capture: false, once: false, passive: false, handler: false });
            st.n_listeners += 1;
            d.own_listeners(st, s.engine(), t);
            d.sched.borrow_mut().ports.get_mut(&port).expect("checked").handler = Some(id);
        }
        (None, None) => {}
    }
    }
    if cb.is_some() {
        lift(s, d.start_port(port))?;
    }
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ MessageEvent

fn msg_event<'s, T>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, f: impl FnOnce(&mut MsgEvent) -> T) -> Res<'s, T> {
    if let Some(o) = obj_id(s, this)
        && let Some(e) = d.sched.borrow_mut().msg_events.get_mut(&o)
    {
        return Ok(f(e));
    }
    Err(s.type_error("Illegal invocation"))
}

/// The local of a raw value that the scheduler holds.
fn local_of<'s>(s: &mut Sc<'s, '_>, v: Value) -> Local<'s> {
    bare_js_api::Engine::raw_local(s.engine(), &v)
}

fn msg_data<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let v = msg_event(d, s, this, |e| e.data)?;
    Ok(local_of(s, v))
}

fn msg_empty<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    msg_event(d, s, this, |_| ())?;
    d.js_str_s(s, "")
}

fn msg_origin<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (win, from) = msg_event(d, s, this, |e| (e.win, e.from.as_ref().map(|f| f.origin.clone())))?;
    d.js_str_s(s, &from.unwrap_or_else(|| if win { d.window_origin() } else { String::new() }))
}

fn msg_source<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (win, from) = msg_event(d, s, this, |e| (e.win, e.from.as_ref().map(|f| f.source)))?;
    Ok(match from {
        Some(Some(src)) => d.obj_local(s, src),
        None if win => s.global(),
        _ => s.null(),
    })
}

fn msg_ports<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    if let Some(v) = msg_event(d, s, this, |e| e.ports)? {
        return Ok(local_of(s, v));
    }
    let moved: Vec<ObjId> = msg_event(d, s, this, |e| e.moved.to_vec())?;
    let items: Vec<Local<'s>> = moved.into_iter().map(|o| d.obj_local(s, o)).collect();
    let arr = s.new_array(&items)?;
    s.set_integrity(arr, true)?;
    let raw = bare_js_api::Engine::raw(s.engine(), arr);
    msg_event(d, s, this, |e| e.ports = Some(raw))?;
    Ok(arr)
}

// ------------------------------------------------------------------------------------------ the driver

impl Browser {
    /// Limits of the scheduler. Set them before scripts run.
    pub fn set_sched_limits(&mut self, l: SchedLimits) {
        self.dom.sched.borrow_mut().limits = l;
    }

    /// Switch to a virtual clock that starts at the current time and moves only with `advance_to` / `advance_time`.
    pub fn use_virtual_clock(&mut self) {
        let now = self.dom.now_ms();
        self.dom.sched.borrow_mut().virtual_now = Some(now);
        // The benchmark clock (`bare_net::vclock`) is one for the process: `Date.now()` reads it too, and a read takes a little time.
        if bare_net::vclock::on() {
            bare_js::eval::set_host_wall_clock(|| {
                bare_net::vclock::advance_by(bare_net::vclock::READ_MS);
                bare_net::vclock::epoch_ms()
            });
            let _ = self.it.set_wall_clock(WallClock::Host);
        }
    }

    /// Like `use_virtual_clock`, starting at `t` ms. `t` must not be before the time the page has seen: the virtual time, or the last
    /// reading of the monotonic clock. Time never goes back for the page. Real time that passed before its first reading is not a
    /// reading, so the start does not depend on how long building the page took.
    pub fn use_virtual_clock_at(&mut self, t: f64) -> R<()> {
        let seen = {
            let s = self.dom.sched.borrow();
            s.virtual_now.unwrap_or(s.last_read.get())
        };
        if !t.is_finite() || t < seen {
            return Err(Error::Unsupported(unsupported("the virtual clock cannot start before the current time")));
        }
        self.dom.sched.borrow_mut().virtual_now = Some(t);
        Ok(())
    }

    /// True after `use_virtual_clock`: the driver moves time, nothing sleeps.
    pub fn uses_virtual_clock(&self) -> bool {
        self.dom.sched.borrow().virtual_now.is_some()
    }

    /// The wall clock that `Date.now()` and `new Date()` read, in ms since 1970-01-01T00:00:00Z. The default is the system clock.
    /// It is separate from the scheduler clock above (`performance.now()`, timers), which counts from the time origin: neither
    /// one moves the other. `use_virtual_clock` does not touch it.
    pub fn wall_clock(&self) -> WallClock {
        self.it.wall_clock()
    }

    /// Fix the wall clock at `epoch_ms` (a whole number of ms within +-8.64e15, else `Unsupported`). It stays there until the driver
    /// sets another value. It may move backward, as a real wall clock can. For deterministic tests.
    pub fn use_virtual_wall_clock_at(&mut self, epoch_ms: f64) -> R<()> {
        self.it.set_wall_clock(WallClock::Virtual(epoch_ms))
    }

    /// Back to the system clock.
    pub fn use_system_wall_clock(&mut self) {
        self.it.set_wall_clock(WallClock::System).expect("the system clock is always valid");
    }

    /// Milliseconds since the time origin (what `performance.now()` gives now).
    pub fn now(&self) -> f64 {
        self.dom.now_ms()
    }

    /// Due time of the earliest queued task.
    pub fn next_scheduled_due(&self) -> Option<f64> {
        let own = self.dom.next_due();
        if !self.dom.kids.any() {
            return own;
        }
        // The tasks of the child pages count too (`browser_child.rs`), on this page's clock.
        match (own, self.dom.kids.next_due()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Does a queued task due at `limit` ms or before keep the page from being settled? See "Settled" in the header.
    pub fn blocks_settle(&self, limit: f64) -> bool {
        let s = self.dom.sched.borrow();
        s.tasks.iter().take_while(|(k, _)| f64::from_bits(k.0) <= limit).any(|(_, t)| !s.skips(t)) || self.dom.kids.blocks(&self.dom, limit)
    }

    /// For `BARE_NET_TRACE`: the first queued task due at `limit` ms or before that blocks the settle, and its due time.
    pub fn settle_blocker(&self, limit: f64) -> Option<(String, f64)> {
        let s = self.dom.sched.borrow();
        let (k, t) = s.tasks.iter().take_while(|(k, _)| f64::from_bits(k.0) <= limit).find(|(_, t)| !s.skips(t))?;
        let what = match t {
            Task::Timer(id) => s.timers.get(id).map_or("timer".into(), |t| format!("timer interval={:?} period={}", t.interval, t.period)),
            t => format!("{t:?}"),
        };
        Some((what, f64::from_bits(k.0)))
    }

    /// What the settle rule skips and leaves queued: the period (ms) of each such recurring timer in due order, and whether a quiet
    /// animation frame loop is queued.
    pub fn idle_left(&self) -> (Vec<u32>, bool, usize) {
        let s = self.dom.sched.borrow();
        let mut timers = Vec::new();
        let mut frames = false;
        let mut idle = 0;
        for t in s.tasks.values() {
            match t {
                Task::Timer(id) if s.skips(t) => timers.extend(s.timers.get(id).map(|t| t.period)),
                Task::Render => frames = s.frame_idle,
                Task::Idle if s.skips(t) => idle = self.dom.idle_count(),
                _ => {}
            }
        }
        (timers, frames, idle)
    }

    pub fn sched_stats(&self) -> SchedStats {
        let s = self.dom.sched.borrow();
        SchedStats {
            timers: s.timers.len(),
            tasks: s.tasks.len(),
            ports: s.ports.len(),
            queued_messages: s.queued,
            queued_message_bytes: s.queued_bytes,
            message_events: s.msg_events.len(),
            halted: s.halted.is_some(),
        }
    }

    /// Run at most `max_tasks` tasks that are due on the current clock, oldest first, then return. It does not wait.
    /// A task that throws is reported and the pump goes on. Any other error ends the pump and every later pump
    /// returns it without running JS. Collect between pumps in a long run (events are freed only by `collect`).
    pub fn run_scheduled_tasks(&mut self, max_tasks: usize) -> R<Pump> {
        self.pump(max_tasks, None)
    }

    /// Virtual clock only: run every task due up to time `t` (ms), stepping the clock to each task's due time,
    /// then set the clock to `t`. Stops early at `max_tasks` (`capped`); call again to go on.
    pub fn advance_to(&mut self, t: f64, max_tasks: usize) -> R<Pump> {
        if !t.is_finite() || t < 0.0 {
            return Err(Error::Unsupported(unsupported("advance_to needs a finite non-negative time")));
        }
        if self.dom.sched.borrow().virtual_now.is_none() {
            return Err(Error::Unsupported(unsupported("advance_to on the monotonic clock")));
        }
        self.pump(max_tasks, Some(t))
    }

    /// `advance_to(now + ms)`.
    pub fn advance_time(&mut self, ms: f64, max_tasks: usize) -> R<Pump> {
        let t = self.dom.now_ms() + ms;
        self.advance_to(t, max_tasks)
    }

    fn pump(&mut self, max: usize, target: Option<f64>) -> R<Pump> {
        let dom = self.dom.clone();
        if let Some(e) = dom.sched.borrow().halted {
            return Err(e);
        }
        let was = std::mem::replace(&mut dom.st.borrow_mut().driver, true);
        let r = self.pump_inner(&dom, max, target);
        dom.st.borrow_mut().driver = was;
        if let Err(e) = r {
            dom.sched.borrow_mut().halted = Some(e);
        }
        r
    }

    fn pump_inner(&mut self, dom: &Dom, max: usize, target: Option<f64>) -> R<Pump> {
        let mut ids = bare_observe::process::context();
        ids.document = self.observe_document;
        let _context = bare_observe::process::enter(ids);
        let _observe = bare_observe::process::span("scheduler", "pump");
        // An empty queue can still carry a sticky failed-job error.
        self.it.host_scope(|it| it.host_checkpoint().map(|_| ()))?;
        let mut ran = 0usize;
        let mut capped = false;
        loop {
            let upto = target.unwrap_or_else(|| dom.now_ms());
            // A rendering step is due at the next frame of the grid after a change that observers could see.
            if dom.frame_wanted() | dom.motion_wants_frame() {
                dom.want_frame()?;
            }
            if dom.scroll_wants_task() {
                dom.schedule(dom.now_ms() + 16.0, Task::Scroll)?;
            }
            if ran >= max {
                capped = dom.next_due().is_some_and(|d| d <= upto);
                break;
            }
            let Some((due, run)) = dom.pop_due(upto) else { break };
            if target.is_some() {
                dom.move_clock(due);
            }
            let counted = !matches!(run, Run::Nothing);
            if counted {
                ran += 1;
            }
            {
                let _task = bare_observe::process::span("scheduler", "task");
                dom.run_task(&mut self.it, run)?;
            }
            // No script runs and no host call is on the stack: the nodes of dead trees can go back to the arena.
            dom.reclaim(false);
            // On the benchmark clock a task takes time: more tasks become due, and a reply that was on its way arrives.
            if counted && bare_net::vclock::on() {
                bare_net::vclock::advance_by(bare_net::vclock::TASK_MS);
            }
        }
        if let Some(t) = target
            && !capped
        {
            dom.move_clock(t);
        }
        bare_observe::process::emit("scheduler", "pump_result", || {
            use bare_observe::{Field, Scalar};
            let s = dom.sched.borrow();
            [Field("ran", Scalar::U64(ran as u64)), Field("capped", Scalar::Bool(capped)),
             Field("tasks", Scalar::U64(s.tasks.len() as u64)), Field("messages", Scalar::U64(s.queued as u64)),
             Field("message_bytes", Scalar::U64(s.queued_bytes as u64))]
        });
        Ok(Pump { ran, capped, next_due: dom.next_due() })
    }
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The nodes that queued tasks name.
pub(crate) fn held(d: &Dom, out: &mut Vec<NodeId>) {
    out.extend(d.sched.borrow().tasks.values().filter_map(|t| match t {
        Task::Select(n) | Task::SelectionChange(n) | Task::Media(n, ..) | Task::Toggle(n, ..) => Some(*n),
        _ => None,
    }));
}
