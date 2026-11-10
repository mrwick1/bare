//! The window on an OS with no backend yet (all but Linux). The types are the same as in `wayland.rs`, so the crates that use
//! them build. `Window::open` fails: a headless program (`bare-drive`, `bare-mcp`) never calls it, and `--window` reports the error.

use crate::{App, Config, Error, Stats};

/// A wake does nothing: no window loop can run here.
#[derive(Clone)]
pub struct Waker(());

impl Waker {
    pub fn wake(&self) {}
}

/// No value of this type can exist: `open` is the only way to make one, and it fails.
pub struct Window {
    never: std::convert::Infallible,
}

impl Window {
    pub fn open(_config: &Config) -> Result<Window, Error> {
        Err(Error(format!("no window backend on {} yet: only Linux (Wayland) has one", std::env::consts::OS)))
    }

    pub fn waker(&self) -> Waker {
        match self.never {}
    }

    pub fn run(self, _app: &mut dyn App) -> Result<Stats, Error> {
        match self.never {}
    }
}
