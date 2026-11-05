//! The Wayland backend: one `xdg_toplevel` that shows two `wl_shm` buffers in turn.
//!
//! Frame lifecycle (the rules are in `redraw.rs` and `slots.rs`):
//!
//! 1. `configure` gives a size. The program draws only after it, and only if the size is new.
//! 2. A frame is drawn when something changed, the last frame is shown (the `wl_surface.frame` callback came), and a
//!    buffer is free. The pixels are copied into the free buffer, which is attached, damaged and committed with a new
//!    frame callback.
//! 3. The buffer stays busy until `wl_buffer.release`. A buffer of a size that is no longer wanted is destroyed only
//!    when it is free and a new frame needs its slot. Until the new frame is ready, the old one stays on screen.
//! 4. The loop sleeps in `poll` on the Wayland socket and on the wake pipe, with no timeout. An idle window uses no CPU.

use crate::redraw::{Redraw, next_size};
use crate::slots::{Pick, Slots};
use crate::{App, Config, Control, Error};
use rustix::event::{PollFd, PollFlags, poll};
use smithay_client_toolkit::reexports::client::{
    Connection, EventQueue, QueueHandle,
    backend::WaylandError,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_output, wl_shm, wl_surface},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    output::{OutputHandler, OutputState},
    delegate_dispatch2, delegate_registry,
    dispatch2::Dispatch2,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window as XdgWindow, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, raw::RawPool},
};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

/// Counts for a test or a measurement. Nothing here costs time in the loop.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Frames committed.
    pub frames: u64,
    /// Buffers made (shared-memory pool and `wl_buffer`). Two for a window that never changes size.
    pub buffers_made: u64,
    /// Times the loop woke up: a Wayland event or a wake. An idle window adds none.
    pub wakeups: u64,
    /// `xdg_toplevel.configure` events.
    pub configures: u64,
}

/// A handle that wakes the loop from another thread and asks for a new frame.
#[derive(Clone)]
pub struct Waker(Arc<UnixStream>);

impl Waker {
    /// Ask for a frame. A call while a wake is waiting costs nothing and adds nothing.
    pub fn wake(&self) {
        // The pipe is non-blocking. `WouldBlock` means that a wake is already waiting: that is enough.
        let _ = (&*self.0).write(&[1]);
    }
}

/// An open window.
pub struct Window {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    wake_rx: UnixStream,
    waker: Waker,
}

struct Buf {
    pool: RawPool,
    buffer: wl_buffer::WlBuffer,
}

impl Drop for Buf {
    fn drop(&mut self) {
        // `WlBuffer` does not destroy itself when dropped. The pool destroys itself and unmaps its memory.
        self.buffer.destroy();
    }
}

struct State {
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    window: XdgWindow,
    slots: Slots,
    bufs: [Option<Buf>; 2],
    redraw: Redraw,
    /// The size of the last frame that the program was asked for.
    drawn: (u32, u32),
    /// The size the compositor wants now.
    want: (u32, u32),
    exit: bool,
    error: Option<String>,
    stats: Stats,
}

impl Window {
    /// Connect to the compositor of `WAYLAND_DISPLAY` and open a window. Nothing is on screen until `run`.
    pub fn open(config: &Config) -> Result<Window, Error> {
        Window::with_connection(config, Connection::connect_to_env().map_err(|e| Error(format!("wayland: {e}")))?)
    }

    /// Open a window on an already connected socket. A test uses this to talk to a compositor of its own.
    pub fn open_on(config: &Config, socket: UnixStream) -> Result<Window, Error> {
        Window::with_connection(config, Connection::from_socket(socket).map_err(|e| Error(format!("wayland: {e}")))?)
    }

    fn with_connection(config: &Config, conn: Connection) -> Result<Window, Error> {
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| err("registry", e))?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh).map_err(|e| err("wl_compositor", e))?;
        let xdg = XdgShell::bind(&globals, &qh).map_err(|e| err("xdg_wm_base", e))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| err("wl_shm", e))?;
        let window = xdg.create_window(compositor.create_surface(&qh), WindowDecorations::RequestServer, &qh);
        window.set_title(config.title.clone());
        window.set_app_id(config.app_id.clone());
        // The first commit has no buffer. It asks the compositor for the first configure.
        window.commit();
        let (wake_rx, wake_tx) = UnixStream::pair().map_err(|e| err("wake pipe", e))?;
        wake_rx.set_nonblocking(true).and_then(|()| wake_tx.set_nonblocking(true)).map_err(|e| err("wake pipe", e))?;
        let state = State {
            registry: RegistryState::new(&globals),
            output: OutputState::new(&globals, &qh),
            shm,
            window,
            slots: Slots::default(),
            bufs: [None, None],
            redraw: Redraw::default(),
            drawn: (0, 0),
            want: config.size,
            exit: false,
            error: None,
            stats: Stats::default(),
        };
        Ok(Window { conn, queue, state, wake_rx, waker: Waker(Arc::new(wake_tx)) })
    }

    pub fn waker(&self) -> Waker {
        self.waker.clone()
    }

    /// Show the window and run until it closes. The thread sleeps in `poll` between events.
    pub fn run(mut self, app: &mut dyn App) -> Result<Stats, Error> {
        let qh = self.queue.handle();
        let fail = |e: WaylandError| Error(format!("wayland: {e}"));
        while !self.state.exit {
            self.queue.dispatch_pending(&mut self.state).map_err(|e| Error(format!("wayland: {e}")))?;
            // One draw for all the events of this round.
            self.state.maybe_draw(app, &qh);
            if let Some(e) = self.state.error.take() {
                return Err(Error(e));
            }
            if self.state.exit {
                break;
            }
            // Send what we wrote. If the socket is full, also wait until it can be written.
            let flushed = match self.conn.flush() {
                Ok(()) => true,
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(e) => return Err(fail(e)),
            };
            // `None`: events were read while we prepared. Dispatch them first.
            let Some(guard) = self.queue.prepare_read() else { continue };
            let wl_fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&wl_fd, if flushed { PollFlags::IN } else { PollFlags::IN | PollFlags::OUT }),
                PollFd::new(&self.wake_rx, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(Error(format!("poll: {e}"))),
            }
            let (wl, wake) = (!fds[0].revents().is_empty(), fds[1].revents().contains(PollFlags::IN));
            self.state.stats.wakeups += 1;
            if wl {
                match guard.read() {
                    Ok(_) => {}
                    Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(fail(e)),
                }
            } else {
                drop(guard);
            }
            if wake {
                let mut sink = [0u8; 64];
                while matches!((&self.wake_rx).read(&mut sink), Ok(n) if n > 0) {}
                self.state.redraw.request();
            }
        }
        let stats = self.state.stats;
        // Destroy the buffers and the window, and tell the compositor before the socket closes.
        drop(self.state);
        let _ = self.conn.flush();
        Ok(stats)
    }
}

impl State {
    fn maybe_draw(&mut self, app: &mut dyn App, qh: &QueueHandle<State>) {
        if !self.redraw.should_draw(self.slots.any_free()) {
            return;
        }
        let size = self.want;
        if size != self.drawn {
            app.resize(size.0, size.1);
            self.drawn = size;
        }
        let mut ctl = Control::default();
        let pixels = app.frame(&mut ctl);
        if pixels.len() != size.0 as usize * size.1 as usize {
            self.error = Some(format!("the frame has {} pixels, the window needs {}x{}", pixels.len(), size.0, size.1));
            return;
        }
        let i = match self.slots.pick(size) {
            Pick::Reuse(i) => i,
            Pick::Recreate(i) => {
                // The slot is free, so the compositor does not read it: destroy the old buffer, then make the new one.
                self.bufs[i] = None;
                match make_buf(&self.shm, size, i, qh) {
                    Ok(buf) => self.bufs[i] = Some(buf),
                    Err(e) => {
                        self.error = Some(format!("shared memory: {e}"));
                        return;
                    }
                }
                self.slots.made(i, size);
                self.stats.buffers_made += 1;
                i
            }
            Pick::Wait => unreachable!("`should_draw` checked that a slot is free"),
        };
        let buf = self.bufs[i].as_mut().expect("the slot has a buffer");
        // Little-endian `0xAARRGGBB` is the memory order of `wl_shm` XRGB8888.
        for (dst, px) in buf.pool.mmap().chunks_exact_mut(4).zip(pixels) {
            dst.copy_from_slice(&px.to_le_bytes());
        }
        let surface = self.window.wl_surface();
        surface.frame(qh, FrameCallbackData(surface.clone()));
        surface.attach(Some(&buf.buffer), 0, 0);
        surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        surface.commit();
        self.slots.attached(i);
        self.redraw.drawn();
        self.stats.frames += 1;
        if ctl.redraw {
            self.redraw.request();
        }
        self.exit |= ctl.close;
    }
}

fn err(what: &str, e: impl std::fmt::Display) -> Error {
    Error(format!("wayland: {what}: {e}"))
}

fn make_buf(shm: &Shm, (w, h): (u32, u32), slot: usize, qh: &QueueHandle<State>) -> Result<Buf, Box<dyn std::error::Error>> {
    let stride = w as i32 * 4;
    let mut pool = RawPool::new(stride as usize * h as usize, shm)?;
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride, wl_shm::Format::Xrgb8888, Slot(slot), qh);
    Ok(Buf { pool, buffer })
}

/// The user data of a `wl_buffer`: the slot that it belongs to.
struct Slot(usize);

impl Dispatch2<wl_buffer::WlBuffer, State> for Slot {
    fn event(&self, state: &mut State, _: &wl_buffer::WlBuffer, event: wl_buffer::Event, _: &Connection, _: &QueueHandle<State>) {
        if let wl_buffer::Event::Release = event {
            state.slots.released(self.0);
        }
    }
}

impl CompositorHandler for State {
    // Paint draws one pixel for each CSS pixel, so the buffer scale stays 1 and the compositor scales the picture.
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.redraw.frame_done();
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &XdgWindow) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &XdgWindow, configure: WindowConfigure, _: u32) {
        self.stats.configures += 1;
        self.want = next_size(configure.new_size, self.want);
        self.redraw.configure(self.want != self.drawn);
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![];
}

delegate_dispatch2!(State);
delegate_registry!(State);
