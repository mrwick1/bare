//! `requestIdleCallback`, `cancelIdleCallback` and `IdleDeadline`. Source: W3C "Cooperative Scheduling of Background Tasks" (idle
//! periods), compared with headless Chrome: `tools/fixtures/ui/idle-cases.txt`, `tools/ui/chrome_topic.mjs`.
//!
//! Headless model (the standard leaves the start of an idle period to the browser)
//! - A request queues one `Task::Idle` at the clock now, unless one is queued. A page that never calls it queues nothing and the pump
//!   pays nothing. The task is an idle period only when no other task is due: it runs after every task that is due at the same time or earlier
//!   (timers, messages, `Task::Render`). If a frame is queued, the period starts after that frame (Chrome runs a rAF callback before an idle
//!   callback, whatever the order of the two calls). A busy frame loop gets a period after each frame, so it cannot starve idle callbacks.
//! - The period ends at the deadline: `min(50 ms, due time of the next task) - now`, on the scheduler clock. `timeRemaining()` is
//!   `max(0, deadline - now)`. On a virtual clock a callback takes no time, so all callbacks of the period run. On the monotonic clock the
//!   period stops when the deadline has passed, and the callbacks that are left wait for the next period.
//! - The callbacks that were queued before the period starts run in the order of the calls, each with a new `IdleDeadline` and a microtask
//!   checkpoint after it. A callback that is added while the period runs, and one that is left over, runs in the next period. It starts at
//!   the deadline (so a callback that asks again gets one period every 50 ms of the clock at most, never a loop in zero time).
//! - `{ timeout }` (`ToUint32`, 0 is none) queues a `Task::IdleTimeout` at `now + timeout`. If the callback is still queued when it runs, the
//!   callback runs as that task with `didTimeout` true and `timeRemaining()` 0. A callback that ran or was cancelled removes its task, so it
//!   does not keep the pump awake.
//! - Handles are numbers from 1, never reused. Callbacks are roots. A throw is reported (`Browser::take_reports`, type `idlecallback`).
//! - `IdleDeadline` is made when the first callback runs, and the global `IdleDeadline` appears then (a lazy accessor would add a third
//!   property to `window` at start, and that cost a frame loop 0.6% of its instructions). Before that `typeof IdleDeadline` is `undefined`
//!   (Chrome: `function`). Its instances are not constructible (`TypeError`). `timeRemaining` and `didTimeout` are prototype members, and the state of an instance is in a
//!   table of the host that the collector prunes.
//!
//! Deviations: Chrome waits for the end of the load before the first idle period, so a short `timeout` there fires before the callback of
//! an earlier request with a longer one. Here the first period comes at once. Bound: `idle-callbacks` (live callbacks).

use crate::browser::*;
use crate::browser_frame::window_this;
use crate::browser_scheduler::{Key, Task};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value, to_uint32};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};

type V = R<Value>;

const MAX_CALLBACKS: usize = 1 << 16;
/// The longest period, in ms of the scheduler clock.
const PERIOD_MS: f64 = 50.0;

#[derive(Default)]
pub(crate) struct Idle {
    /// Live callbacks by handle, with the key of their timeout task.
    cbs: RefCell<BTreeMap<u32, (Value, Option<Key>)>>,
    next: Cell<u32>,
    /// A `Task::Idle` is queued.
    queued: Cell<bool>,
    /// The queued period was moved behind the frame once.
    deferred: Cell<bool>,
    /// `(prototype, constructor)` of `IdleDeadline`, once made.
    made: Cell<Option<(ObjId, ObjId)>>,
    /// `IdleDeadline` instances: `(deadline, didTimeout)`.
    deadlines: RefCell<HashMap<ObjId, (f64, bool)>>,
}

fn request(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    const F: &str = "Failed to execute 'requestIdleCallback' on 'Window'";
    window_this(d, it, this)?;
    let Some(&cb) = a.first() else {
        return Err(it.type_error(&format!("{F}: 1 argument required, but only 0 present.")));
    };
    if !it.is_callable_value(cb) {
        return Err(it.type_error(&format!("{F}: The callback provided as parameter 1 is not a function.")));
    }
    let timeout = match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => 0,
        o @ Value::Object(_) => {
            let k = d.key(it, "timeout")?;
            match it.get_value(o, k)? {
                Value::Undefined => 0,
                v => to_uint32(it.to_js_number(v)?),
            }
        }
        _ => return Err(it.type_error(&format!("{F}: parameter 2 ('options') is not an object."))),
    };
    let i = &d.idle;
    if i.cbs.borrow().len() >= MAX_CALLBACKS {
        return Err(d.lim("idle-callbacks"));
    }
    let id = i.next.get().checked_add(1).ok_or_else(|| d.lim("idle-handles"))?;
    let now = d.now_ms();
    let key = if timeout > 0 { Some(d.schedule(now + timeout as f64, Task::IdleTimeout(id))?) } else { None };
    if let Err(e) = d.idle_queue(now) {
        if let Some(k) = key {
            d.unschedule(k);
        }
        return Err(e);
    }
    d.idle_asked();
    i.next.set(id);
    i.cbs.borrow_mut().insert(id, (cb, key));
    Ok(Value::Number(id as f64))
}

fn cancel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    window_this(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'cancelIdleCallback' on 'Window': 1 argument required, but only 0 present."));
    }
    let n = it.to_js_number(a[0])?;
    let gone = d.idle.cbs.borrow_mut().remove(&to_uint32(n));
    if let Some((_, Some(k))) = gone {
        d.unschedule(k);
    }
    Ok(Value::Undefined)
}

fn deadline_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(f64, bool)> {
    match this {
        Value::Object(o) if let Some(&v) = d.idle.deadlines.borrow().get(&o) => Ok(v),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn time_remaining(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (end, _) = deadline_of(d, it, this)?;
    Ok(Value::Number((end - d.now_ms()).max(0.0)))
}

fn did_timeout(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(Value::Bool(deadline_of(d, it, this)?.1))
}

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Illegal constructor"))
}

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.idle.made.get().is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let ctor = d.add_fn(it, illegal, "IdleDeadline", 0, true)?;
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(ctor), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "IdleDeadline")?;
    d.def_method(it, proto, "timeRemaining", 0, time_remaining)?;
    d.def_get(it, proto, "didTimeout", did_timeout, None)?;
    d.idle.made.set(Some((proto, ctor)));
    // The global is made here and not as a lazy accessor: one more property of `window` costs a frame loop 0.6% (see resources.md).
    let (g, k) = (it.global_object(), d.key(it, "IdleDeadline")?);
    if matches!(it.get_value(Value::Object(g), k)?, Value::Undefined) {
        it.define_data(g, k, Value::Object(ctor), true, false, true)?;
    }
    Ok(())
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    for (name, h) in [("requestIdleCallback", request as H), ("cancelIdleCallback", cancel)] {
        let f = d.add_fn(it, h, name, 1, false)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, true, true)?;
    }
    Ok(())
}

impl Dom {
    pub(crate) fn idle_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.idle.cbs.borrow().values().map(|c| c.0));
        if let Some((p, c)) = self.idle.made.get() {
            out.extend([Value::Object(p), Value::Object(c)]);
        }
    }

    pub(crate) fn idle_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        self.idle.deadlines.borrow_mut().retain(|&o, _| live(o));
    }

    /// Queue the next idle period at `due` unless one is queued.
    pub(crate) fn idle_count(&self) -> usize {
        self.idle.cbs.borrow().len()
    }

    fn idle_queue(&self, due: f64) -> R<()> {
        if !self.idle.queued.get() {
            self.schedule(due, Task::Idle)?;
            self.idle.queued.set(true);
        }
        Ok(())
    }

    /// Call one callback with a new `IdleDeadline`, then a microtask checkpoint. A throw is reported.
    fn idle_call(&self, it: &mut Interp<'_>, cb: Value, end: f64, timed_out: bool) -> R<()> {
        ensure(self, it)?;
        let proto = self.idle.made.get().expect("made").0;
        if self.idle.deadlines.borrow().len() >= MAX_CALLBACKS {
            return Err(self.lim("idle-deadlines"));
        }
        let o = self.obj(it, proto)?;
        self.idle.deadlines.borrow_mut().insert(o, (end, timed_out));
        it.charge(1)?;
        match it.call(cb, Value::Undefined, &[Value::Object(o)]) {
            Ok(_) => {}
            Err(Error::Throw(v)) => self.report(it, v, "idlecallback", Target::Window),
            Err(e) => return Err(e),
        }
        it.host_checkpoint().map(|_| ())
    }

    /// `Task::Idle`: see the header.
    /// True when a period ran callbacks.
    pub(crate) fn idle_run(&self, it: &mut Interp<'_>) -> R<bool> {
        let i = &self.idle;
        i.queued.set(false);
        if i.cbs.borrow().is_empty() {
            return Ok(false);
        }
        let now = self.now_ms();
        let next = self.next_due();
        if next.is_some_and(|t| t <= now) {
            return self.idle_queue(now).map(|_| false);
        }
        if !i.deferred.get() && self.frame.queued.get() {
            i.deferred.set(true);
            return self.idle_queue(self.frame.due.get()).map(|_| false);
        }
        i.deferred.set(false);
        // A request during the period waits for the next one: the period counts as queued until it ends.
        i.queued.set(true);
        let end = now + next.map_or(PERIOD_MS, |t| (t - now).min(PERIOD_MS));
        let upto = i.next.get();
        let mut first = true;
        while first || self.now_ms() < end {
            first = false;
            let (cb, key) = {
                let mut c = i.cbs.borrow_mut();
                match c.first_entry() {
                    Some(e) if *e.key() <= upto => e.remove(),
                    _ => break,
                }
            };
            if let Some(k) = key {
                self.unschedule(k);
            }
            self.idle_call(it, cb, end, false)?;
        }
        i.queued.set(false);
        if !i.cbs.borrow().is_empty() {
            self.idle_queue(end.max(self.now_ms()))?;
        }
        Ok(true)
    }

    /// `Task::IdleTimeout`: the callback runs now if it still waits.
    pub(crate) fn idle_timeout(&self, it: &mut Interp<'_>, id: u32) -> R<bool> {
        let gone = self.idle.cbs.borrow_mut().remove(&id);
        match gone {
            Some((cb, _)) => self.idle_call(it, cb, self.now_ms(), true).map(|_| true),
            None => Ok(false),
        }
    }
}
