//! The window: a small interface that shows a pixel buffer. `decisions.md` ("Platforms") asks for this.
//!
//! The program implements `App`. It says what size the picture has (`resize`) and gives the picture (`frame`: one
//! `u32` per pixel, `0xAARRGGBB`, the alpha is ignored). The window decides *when* to ask for it: once after the first
//! size, once after each new size, and once after each `Control::redraw` or `Waker::wake`. It never asks twice for the
//! same change, and it asks at most once for each frame that the compositor shows.
//!
//! The only backend is Wayland (`wayland.rs`, with `wl_shm` buffers). A port to another OS adds a module with the same
//! `Window`, `Waker` and `Stats`, and nothing else changes. `redraw.rs` and `slots.rs` hold the rules that do not touch
//! the OS (when to draw, which buffer to use), so a port keeps them.
//!
//! Input (S4 topic 4): `App::event` gets the pointer and keyboard events (`input.rs`). The pointer position is in window
//! pixels. A key has its DOM `key` and `code` (the layout is read with xkbcommon, through smithay-client-toolkit), and the
//! window repeats a held key by itself. An event can ask for a new frame (`Control::redraw`) and for a cursor shape
//! (`Control::set_cursor`).
//!
//! Timers (S5): `App::tick` runs the work that is due and gives the time of the next work. The loop sleeps in `poll` until that
//! time at the latest, so page timers and `fetch` tasks run between input events. With no work due, the timeout is none.
//!
//! Not here yet: a fractional or integer HiDPI scale (paint draws one pixel for each CSS pixel, see `docs/resources.md`,
//! "Wayland window"), scroll events, touch, a text input method (IME) and the clipboard.

pub mod input;
pub mod redraw;
pub mod slots;
mod wayland;

pub use input::{Cursor, Event, Mods};
pub use wayland::{Stats, Waker, Window};

/// What the program asks of the window for the frame in progress.
#[derive(Debug, Default)]
pub struct Control {
    pub(crate) close: bool,
    pub(crate) redraw: bool,
    pub(crate) cursor: Option<Cursor>,
}

impl Control {
    /// Close the window when this call returns.
    pub fn close(&mut self) {
        self.close = true;
    }

    /// Draw one more frame, after the compositor shows this one (an animation asks again on each frame).
    pub fn redraw(&mut self) {
        self.redraw = true;
    }

    /// Show this cursor shape over the window from now on. The window keeps it until the next call.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = Some(cursor);
    }
}

/// The program that the window shows.
pub trait App {
    /// The window is `width` by `height` pixels now. Called before `frame`, once for each new size (and for the first).
    fn resize(&mut self, width: u32, height: u32);

    /// The picture at the size of the last `resize`: exactly `width * height` pixels, row by row from the top.
    fn frame(&mut self, ctl: &mut Control) -> &[u32];

    /// A pointer or keyboard event. All the events of one round of the loop come before the next `frame`, so a burst of
    /// events makes one frame. Ask for one with `ctl.redraw()` when the event changed what the picture shows.
    fn event(&mut self, event: Event, ctl: &mut Control) {
        let _ = (event, ctl);
    }

    /// Run the work that is due now: the timers of a page, the jobs of a test driver. Gives when work is due next; `None` means only
    /// after an event or a wake. The loop calls it once in each round, after the events and before the draw, and sleeps until that
    /// time at the latest. Ask for a frame with `ctl.redraw()` when the work changed the picture.
    fn tick(&mut self, ctl: &mut Control) -> Option<std::time::Instant> {
        let _ = ctl;
        None
    }

    /// The loop ends a round and goes to sleep. `busy` is the time since it woke: the events, `tick` and the draw of the round.
    fn round(&mut self, busy: std::time::Duration) {
        let _ = busy;
    }
}

/// What a window needs to open.
#[derive(Debug, Clone)]
pub struct Config {
    pub title: String,
    pub app_id: String,
    /// The size to use when the compositor leaves it to us.
    pub size: (u32, u32),
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}
