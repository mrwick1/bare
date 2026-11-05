//! When to draw. The rules are plain logic, so a test can check them with no compositor.
//!
//! A frame is drawn only if something changed (`dirty`), the window has a size (`configured`), the compositor has shown
//! the last frame (`frame_pending` is false) and a buffer is free. No timer and no polling: each of these four facts
//! changes only when the compositor sends an event or the program calls `request`.

use std::num::NonZeroU32;

/// The largest side of a window, in pixels. A configure above it is cut: 8192 by 8192 is 256 MB for each buffer.
pub const MAX_SIDE: u32 = 8192;

#[derive(Debug, Default)]
pub struct Redraw {
    configured: bool,
    dirty: bool,
    frame_pending: bool,
}

impl Redraw {
    /// The compositor sent a size. Something changed only if the size is new, or if this is the first configure.
    pub fn configure(&mut self, size_changed: bool) {
        if size_changed || !self.configured {
            self.dirty = true;
        }
        self.configured = true;
    }

    /// The program wants a new picture.
    pub fn request(&mut self) {
        self.dirty = true;
    }

    /// The compositor showed the last frame (the `wl_surface.frame` callback).
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
    }

    /// Draw now? `buffer_free` is true when a buffer exists that the compositor does not hold.
    pub fn should_draw(&self, buffer_free: bool) -> bool {
        self.configured && self.dirty && !self.frame_pending && buffer_free
    }

    /// A frame was committed with a frame callback.
    pub fn drawn(&mut self) {
        self.dirty = false;
        self.frame_pending = true;
    }
}

/// The size to draw for a configure. A side that the compositor leaves out (`None`) keeps its old size, and the first
/// time it is the size that the program asked for. A side is never 0 and never above `MAX_SIDE`.
pub fn next_size(configure: (Option<NonZeroU32>, Option<NonZeroU32>), current: (u32, u32)) -> (u32, u32) {
    let side = |new: Option<NonZeroU32>, old: u32| new.map_or(old, NonZeroU32::get).clamp(1, MAX_SIDE);
    (side(configure.0, current.0), side(configure.1, current.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(n)
    }

    #[test]
    fn nothing_is_drawn_before_the_first_configure() {
        let mut r = Redraw::default();
        r.request();
        assert!(!r.should_draw(true));
        r.configure(true);
        assert!(r.should_draw(true));
    }

    #[test]
    fn one_frame_waits_for_the_callback_of_the_one_before() {
        let mut r = Redraw::default();
        r.configure(true);
        r.drawn();
        r.request();
        assert!(!r.should_draw(true), "the last frame is not shown yet");
        r.frame_done();
        assert!(r.should_draw(true));
    }

    #[test]
    fn an_idle_window_never_draws() {
        let mut r = Redraw::default();
        r.configure(true);
        r.drawn();
        r.frame_done();
        assert!(!r.should_draw(true));
        // The same size again (a focus change, for example) is not a change.
        r.configure(false);
        assert!(!r.should_draw(true));
    }

    #[test]
    fn a_new_size_asks_for_a_frame_and_a_busy_buffer_waits() {
        let mut r = Redraw::default();
        r.configure(true);
        r.drawn();
        r.frame_done();
        r.configure(true);
        assert!(!r.should_draw(false), "both buffers are held: wait for a release");
        assert!(r.should_draw(true));
    }

    #[test]
    fn many_requests_make_one_frame() {
        let mut r = Redraw::default();
        r.configure(true);
        for _ in 0..10 {
            r.request();
        }
        assert!(r.should_draw(true));
        r.drawn();
        r.frame_done();
        assert!(!r.should_draw(true));
    }

    #[test]
    fn size_keeps_a_side_that_is_left_out_and_clamps() {
        assert_eq!(next_size((None, None), (1280, 800)), (1280, 800));
        assert_eq!(next_size((nz(640), None), (1280, 800)), (640, 800));
        assert_eq!(next_size((nz(1), nz(1)), (1280, 800)), (1, 1));
        assert_eq!(next_size((nz(100_000), nz(9000)), (1280, 800)), (MAX_SIDE, MAX_SIDE));
        assert_eq!(next_size((None, None), (0, 0)), (1, 1));
    }
}
