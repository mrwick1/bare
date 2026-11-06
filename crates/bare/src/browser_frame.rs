//! `requestAnimationFrame`, `cancelAnimationFrame` and the rendering step of the host.
//! Source: HTML Standard, "Animation frames" and "update the rendering" (read by hand, then compared with headless Chrome:
//! `tools/fixtures/ui/raf-cases.txt`, `tools/ui/chrome_topic.mjs`).
//!
//! When a frame happens
//! - The host has no display and no vsync. A frame is one `Task::Render` of the scheduler (`browser_scheduler.rs`). It is due at the next
//!   multiple of 1000/60 ms of the scheduler clock (the frame grid). It is queued when a frame is wanted and none is queued: by
//!   `requestAnimationFrame`, by an observer that has a reason to look again (`ResizeObserver`, `IntersectionObserver`), never otherwise.
//!   So an idle page queues no frame and a session can settle. A page that asks for a frame in each frame gets 60 frames per second of
//!   virtual time, one frame per 16.67 ms step of `Browser::advance_time`, and no frame while the driver does not move the clock.
//! - The timestamp of a frame is the clock when it starts (`performance.now()` there, the same clock as `Event.timeStamp`). Every callback
//!   of one frame gets the same number.
//!
//! One frame, in the order of the standard
//! 1. The animation frame callbacks that were queued before the frame started, in the order of the calls. A callback that is added while the
//!    frame runs waits for the next frame. A callback that is cancelled while the frame runs does not run, also if it was queued before.
//!    Each callback runs in its own task-like step: a throw is reported (`Browser::take_reports`, type `animationframe`) and the frame goes on,
//!    and a microtask checkpoint follows each callback.
//! 2. `ResizeObserver` (`browser_resize.rs`). 3. `IntersectionObserver` (`browser_intersection.rs`).
//!
//! Handles are numbers from 1, never reused. Gates: none. Bound: `animation-frame-callbacks` (live callbacks).
//! The two globals are plain data properties of `window`. Not here: `requestIdleCallback`, `document.timeline`, `Animation`.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::{Value, to_uint32};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

type V = R<Value>;

/// One frame is 1000/60 ms.
pub(crate) const FRAME_MS: f64 = 1000.0 / 60.0;
const MAX_CALLBACKS: usize = 1 << 16;

#[derive(Default)]
pub(crate) struct Frame {
    /// Live callbacks by handle: the order of the handles is the order of the calls.
    cbs: RefCell<BTreeMap<u32, Value>>,
    next: Cell<u32>,
    /// A `Task::Render` is queued.
    pub(crate) queued: Cell<bool>,
}

fn window_this(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match this {
        Value::Undefined | Value::Null => Ok(()),
        Value::Object(o) if Some(&o) == d.global.get() => Ok(()),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn request(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    window_this(d, it, this)?;
    let Some(&cb) = a.first() else {
        return Err(it.type_error("Failed to execute 'requestAnimationFrame' on 'Window': 1 argument required, but only 0 present."));
    };
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to execute 'requestAnimationFrame' on 'Window': parameter 1 is not of type 'Function'."));
    }
    let f = &d.frame;
    if f.cbs.borrow().len() >= MAX_CALLBACKS {
        return Err(d.lim("animation-frame-callbacks"));
    }
    let id = f.next.get().checked_add(1).ok_or_else(|| d.lim("animation-frame-handles"))?;
    d.want_frame()?;
    f.next.set(id);
    f.cbs.borrow_mut().insert(id, cb);
    Ok(Value::Number(id as f64))
}

fn cancel(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    window_this(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'cancelAnimationFrame' on 'Window': 1 argument required, but only 0 present."));
    }
    let n = it.to_js_number(a[0])?;
    d.frame.cbs.borrow_mut().remove(&to_uint32(n));
    Ok(Value::Undefined)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    for (name, h) in [("requestAnimationFrame", request as H), ("cancelAnimationFrame", cancel)] {
        let f = d.add_fn(it, h, name, 1, false)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, true, true)?;
    }
    Ok(())
}

impl Dom {
    pub(crate) fn frame_roots(&self, out: &mut Vec<Value>) {
        out.extend(self.frame.cbs.borrow().values().copied());
    }

    /// Does an observer want a frame now? (A page with no observer pays two borrows.)
    pub(crate) fn frame_wanted(&self) -> bool {
        self.resize_wants_frame() | self.io_wants_frame()
    }

    /// The rendering step: callbacks, then the observers, in the order of the standard (see the header).
    pub(crate) fn frame_run(&self, it: &mut Interp<'_>) -> R<()> {
        let ts = Value::Number(self.now_ms());
        // Handles below `upto` were queued before this frame.
        let upto = self.frame.next.get();
        loop {
            let cb = {
                let mut c = self.frame.cbs.borrow_mut();
                match c.first_entry() {
                    Some(e) if *e.key() <= upto => e.remove(),
                    _ => break,
                }
            };
            it.charge(1)?;
            match it.call(cb, Value::Undefined, &[ts]) {
                Ok(_) => {}
                Err(Error::Throw(v)) => self.report(it, v, "animationframe", Target::Window),
                Err(e) => return Err(e),
            }
            it.host_checkpoint()?;
        }
        self.resize_frame(it)?;
        self.io_frame(it)?;
        if !self.frame.cbs.borrow().is_empty() {
            self.want_frame()?;
        }
        Ok(())
    }
}
