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
//! The settle rule treats a rendering step as quiet when its callbacks and observers changed the DOM at most 8 times and queued nothing
//! but the next step (`Sched::frame_idle`, `browser_scheduler.rs`). A loop of such steps does not keep a session from settling.
//!
//! Handles are numbers from 1, never reused. Gates: none. Bound: `animation-frame-callbacks` (live callbacks).
//! The two globals are plain data properties of `window`. `requestIdleCallback` is in `browser_idle.rs`. Not here: `document.timeline`, `Animation`.

use crate::eng::{Eng, Fault, Ops};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_scoped::*;
use crate::js_num::to_uint32;
use bare_js_api::{Args, Attrs, Engine, HostFn, Local};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

/// One frame is 1000/60 ms.
pub(crate) const FRAME_MS: f64 = 1000.0 / 60.0;
const MAX_CALLBACKS: usize = 1 << 16;

#[derive(Default)]
pub(crate) struct Frame<E: Eng> {
    /// Live callbacks by handle: the order of the handles is the order of the calls.
    cbs: RefCell<BTreeMap<u32, <E as Eng>::Raw>>,
    next: Cell<u32>,
    /// A `Task::Render` is queued.
    pub(crate) queued: Cell<bool>,
    /// Due time of the queued `Task::Render`.
    pub(crate) due: Cell<f64>,
}

pub(crate) fn window_this<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, ()> {
    let ok = match obj_id::<E>(s, this) {
        Some(o) => Some(&o) == d.global.get() || crate::browser_frames::is_blank_window(d, o),
        None => is_nullish::<E>(s, this),
    };
    if ok { Ok(()) } else { Err(s.type_error("Illegal invocation")) }
}

fn request<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    window_this(d, s, this)?;
    let Some(cb) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'requestAnimationFrame' on 'Window': 1 argument required, but only 0 present."));
    };
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'requestAnimationFrame' on 'Window': parameter 1 is not of type 'Function'."));
    }
    let cb = s.engine().raw(cb);
    let f = &d.frame;
    if f.cbs.borrow().len() >= MAX_CALLBACKS {
        return Err(d.lim_s(s, "animation-frame-callbacks"));
    }
    let Some(id) = f.next.get().checked_add(1) else { return Err(d.lim_s(s, "animation-frame-handles")) };
    lift::<E, _>(s, d.want_frame())?;
    f.next.set(id);
    f.cbs.borrow_mut().insert(id, cb);
    Ok(s.number(id as f64))
}

fn cancel<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    window_this(d, s, this)?;
    let Some(v) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'cancelAnimationFrame' on 'Window': 1 argument required, but only 0 present."));
    };
    let n = s.to_number(v)?;
    d.frame.cbs.borrow_mut().remove(&to_uint32(n));
    Ok(s.undefined())
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    for (name, h) in [("requestAnimationFrame", request as HS<E>), ("cancelAnimationFrame", cancel)] {
        let f = d.add_fn_s(s, h, name, 1, HostFn::Method)?;
        s.define_str(g, name, f, Attrs::ALL)?;
    }
    Ok(())
}

impl<E: Eng> Dom<E> {
    pub(crate) fn frame_roots(&self, out: &mut Roots<'_, '_, E>) {
        out.extend(self.frame.cbs.borrow().values().copied());
    }

    /// Does an observer want a frame now? (A page with no observer pays two borrows.)
    pub(crate) fn frame_wanted(&self) -> bool {
        self.resize_wants_frame() | self.io_wants_frame()
    }

    /// The rendering step: callbacks, then the observers, in the order of the standard (see the header).
    pub(crate) fn frame_run(&self, it: &mut E::Vm) -> R<E, ()> {
        let ts = E::num(self.now_ms());
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
            it.charge_o(1)?;
            match it.call_o(cb, E::undef(), &[ts]) {
                Ok(_) => {}
                Err(Fault::Throw(v)) => self.report(it, v, "animationframe", Target::Window),
                Err(e) => return Err(e),
            }
            it.host_checkpoint()?;
        }
        self.motion_frame(it)?;
        self.resize_frame(it)?;
        self.io_frame(it)?;
        if !self.frame.cbs.borrow().is_empty() {
            self.want_frame()?;
        }
        Ok(())
    }
}
