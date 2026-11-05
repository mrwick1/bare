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
//! Not here yet: input (S4 topic 4), a fractional or integer HiDPI scale (paint draws one pixel for each CSS pixel, see
//! `docs/resources.md`, "Wayland window"), and clipboard and cursor shapes.

pub mod redraw;
pub mod slots;
mod wayland;

pub use wayland::{Stats, Waker, Window};

/// What the program asks of the window for the frame in progress.
#[derive(Debug, Default)]
pub struct Control {
    pub(crate) close: bool,
    pub(crate) redraw: bool,
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
}

/// The program that the window shows.
pub trait App {
    /// The window is `width` by `height` pixels now. Called before `frame`, once for each new size (and for the first).
    fn resize(&mut self, width: u32, height: u32);

    /// The picture at the size of the last `resize`: exactly `width * height` pixels, row by row from the top.
    fn frame(&mut self, ctl: &mut Control) -> &[u32];
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
