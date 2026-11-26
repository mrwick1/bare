//! The Wayland backend: one `xdg_toplevel` that shows two `wl_shm` buffers in turn.
//!
//! Frame lifecycle (the rules are in `redraw.rs` and `slots.rs`):
//!
//! 1. `configure` gives a size. The program draws only after it, and only if the size is new.
//! 2. A frame is drawn when something changed, the last frame is shown (the `wl_surface.frame` callback came), and a
//!    buffer is free. The program draws into the free buffer, which is attached, damaged and committed with a new frame
//!    callback. A program that keeps its frame (`App::frame` gives `false`) commits nothing.
//! 3. The buffer stays busy until `wl_buffer.release`. A buffer of a size that is no longer wanted is destroyed only
//!    when it is free and a new frame needs its slot. Until the new frame is ready, the old one stays on screen.
//! 4. The loop sleeps in `poll` on the Wayland socket and on the wake pipe. The timeout is the next work of `App::tick` (a page
//!    timer), a key repeat, or the drop of the free buffer `IDLE` after the last frame, and none when none waits. An idle
//!    window uses no CPU, and holds one buffer: the one that the compositor shows.
//!
//! Clipboard: the seat also gives a data device. `Control::set_clipboard` makes a data source that the window owns, and the window
//! writes its data to the pipes of the programs that paste (`Send`). `Control::read_clipboard` reads the offer of the selection, one
//! MIME type after the other (`ReadJob`). The pipe fds join the `poll` set only while a read or a write is open, so an idle window
//! still wakes for nothing. A read waits for `poll` to say that the pipe is readable, so the blocking `read` does not block. A write
//! sends 4096 bytes at most after `poll` says that the pipe is writable: a pipe that has room for one page takes that atomically.
//!
//! Input: the seat gives a pointer and a keyboard (smithay-client-toolkit, with xkbcommon for the layout). Their handlers
//! only queue `Event`s in `State::pending`. The loop hands the queue to `App::event` after the Wayland events of the round
//! are read, and before the draw, so many events make one frame. The one timer is the key repeat: while a key is held, the
//! `poll` timeout is the time to the next repeat (`input::Repeat`), and it is `None` again as soon as the key goes up.

use crate::clipboard::{ReadJob, offer_for};
use crate::input::{self, Cursor, Event, Mods, Repeat};
use crate::redraw::{Redraw, next_size};
use crate::slots::{Pick, Slots};
use crate::{App, Config, Control, Error, Stats};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use smithay_client_toolkit::reexports::client::{
    Connection, EventQueue, QueueHandle,
    backend::WaylandError,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::seat::{
    Capability, SeatHandler, SeatState,
    keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers, RepeatInfo},
    pointer::{AxisScroll, CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer},
};
use smithay_client_toolkit::data_device_manager::{
    DataDeviceManagerState, ReadPipe, WritePipe,
    data_device::{DataDevice, DataDeviceHandler},
    data_offer::{DataOfferHandler, DragOffer, SelectionOffer},
    data_source::{CopyPasteSource, DataSourceHandler},
};
use smithay_client_toolkit::reexports::client::protocol::{wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource};
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
use std::time::{Duration, Instant};

/// An idle window drops its free buffer after this time with no frame. The compositor holds the other one, the frame on screen.
const IDLE: Duration = Duration::from_secs(1);

/// A paste bigger than this is dropped: a source that never ends must not fill the memory.
const MAX_PASTE: usize = 64 << 20;
/// What a write to a pipe sends at once (`PIPE_BUF`).
const CHUNK: usize = 4096;

/// The read of one MIME type of the selection that is open.
struct Reading {
    job: ReadJob,
    offer: SelectionOffer,
    /// The type, the pipe and the bytes so far.
    cur: Option<(String, ReadPipe, Vec<u8>)>,
}

/// Our data on its way to a program that pastes.
struct Send {
    pipe: WritePipe,
    data: Arc<[u8]>,
    done: usize,
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
    seat: SeatState,
    compositor: CompositorState,
    conn: Connection,
    shm: Shm,
    window: XdgWindow,
    pointer: Option<ThemedPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    qh: QueueHandle<State>,
    /// The clipboard. Each part is `None` when the compositor has no data device manager (or no seat yet).
    ddm: Option<DataDeviceManagerState>,
    device: Option<DataDevice>,
    /// The source that we own and its data. The source is `None` after another program copied.
    source: Option<CopyPasteSource>,
    own: Vec<(String, Arc<[u8]>)>,
    /// The serial of the last key press: the compositor wants one for `set_selection`.
    serial: u32,
    reading: Option<Reading>,
    writes: Vec<Send>,
    /// Events that the handlers queued, for `App::event`.
    pending: Vec<Event>,
    mods: Mods,
    /// DOM `buttons` bit mask of the held buttons, and the last pointer position.
    buttons: u16,
    pos: (f64, f64),
    repeat: Repeat,
    /// The key that repeats: its scancode, keysym and text.
    repeating: Option<(u32, Keysym, Option<String>)>,
    cursor: Cursor,
    slots: Slots,
    bufs: [Option<Buf>; 2],
    redraw: Redraw,
    /// The size of the last frame that the program was asked for.
    drawn: (u32, u32),
    /// The size of the frame that the last commit showed.
    shown: Option<(u32, u32)>,
    /// When the last frame was committed, until the free buffer is dropped (`IDLE`).
    drawn_at: Option<Instant>,
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
            seat: SeatState::new(&globals, &qh),
            compositor,
            conn: conn.clone(),
            shm,
            window,
            pointer: None,
            keyboard: None,
            qh: qh.clone(),
            // A compositor with no data device manager still gets a window: copy and paste do nothing.
            ddm: DataDeviceManagerState::bind(&globals, &qh).ok(),
            device: None,
            source: None,
            own: Vec::new(),
            serial: 0,
            reading: None,
            writes: Vec::new(),
            pending: Vec::new(),
            mods: Mods::default(),
            buttons: 0,
            pos: (0.0, 0.0),
            // The defaults of most toolkits, until the compositor gives its own with `repeat_info`.
            repeat: Repeat::new(25, 600),
            repeating: None,
            cursor: Cursor::Default,
            slots: Slots::default(),
            bufs: [None, None],
            redraw: Redraw::default(),
            drawn: (0, 0),
            shown: None,
            drawn_at: None,
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
        let mut woke = Instant::now();
        while !self.state.exit {
            self.queue.dispatch_pending(&mut self.state).map_err(|e| Error(format!("wayland: {e}")))?;
            self.state.repeat_due();
            self.state.drop_idle(Instant::now());
            self.state.deliver(app);
            let mut ctl = Control::default();
            let next = app.tick(&mut ctl);
            self.state.obey(ctl);
            // One draw for all the events of this round.
            self.state.maybe_draw(app, &qh);
            if let Some(e) = self.state.error.take() {
                return Err(Error(e));
            }
            if self.state.exit {
                break;
            }
            // A clipboard answer that `obey` queued is delivered in the next round, not after a sleep.
            if !self.state.pending.is_empty() {
                continue;
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
            let wl_flags = if flushed { PollFlags::IN } else { PollFlags::IN | PollFlags::OUT };
            let mut base;
            let mut more;
            // The pipes of the clipboard are in the set only while one is open.
            let fds: &mut [PollFd] = if self.state.io_open() {
                more = vec![PollFd::new(&wl_fd, wl_flags), PollFd::new(&self.wake_rx, PollFlags::IN)];
                self.state.io_fds(&mut more);
                &mut more
            } else {
                base = [PollFd::new(&wl_fd, wl_flags), PollFd::new(&self.wake_rx, PollFlags::IN)];
                &mut base
            };
            // Sleep until an event, the next key repeat when a key is held, or the next work of the app.
            let now = Instant::now();
            app.round(now - woke);
            let idle = self.state.idle_at().map(|t| t.saturating_duration_since(now));
            let wait = [self.state.repeat.wait(now), next.map(|t| t.saturating_duration_since(now)), idle].into_iter().flatten().min();
            let timeout = wait.map(|d| Timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ });
            let polled = poll(fds, timeout.as_ref());
            woke = Instant::now();
            match polled {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(Error(format!("poll: {e}"))),
            }
            let (wl, wake) = (!fds[0].revents().is_empty(), fds[1].revents().contains(PollFlags::IN));
            let io: Vec<PollFlags> = fds[2..].iter().map(|f| f.revents()).collect();
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
            if !io.is_empty() {
                self.state.io_ready(&io);
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
    /// Queue the repeats that are due.
    fn repeat_due(&mut self) {
        let now = Instant::now();
        while let Some(code) = self.repeat.due(now) {
            match &self.repeating {
                Some((c, sym, text)) if *c == code => {
                    let ev = self.key_event(code, *sym, text.clone(), true, true);
                    self.pending.push(ev);
                }
                _ => self.repeat.clear(),
            }
        }
    }

    fn key_event(&self, scancode: u32, sym: Keysym, text: Option<String>, pressed: bool, repeat: bool) -> Event {
        let m = self.mods;
        // The text of a key is typed text only: no control characters, and none while ctrl, alt or the logo key is held.
        let text = text.filter(|t| pressed && !t.is_empty() && !t.chars().any(char::is_control) && !m.ctrl && !m.alt && !m.meta);
        Event::Key { key: input::key_of(sym.raw(), text.as_deref()), code: input::code_of(scancode), pressed, repeat, text, mods: m }
    }

    /// Give the queued events to the app, one by one, and do what each asks for.
    fn deliver(&mut self, app: &mut dyn App) {
        for ev in std::mem::take(&mut self.pending) {
            let mut ctl = Control::default();
            app.event(ev, &mut ctl);
            self.obey(ctl);
        }
    }

    /// When the free buffer goes, if one exists and no frame came since the last one.
    fn idle_at(&self) -> Option<Instant> {
        self.drawn_at.filter(|_| self.slots.free_made().is_some()).map(|t| t + IDLE)
    }

    /// Drop the free buffer of an idle window. The memory of the pool goes back to the system; the next frame makes a new one.
    fn drop_idle(&mut self, now: Instant) {
        if self.idle_at().is_some_and(|t| t <= now) {
            while let Some(i) = self.slots.free_made() {
                self.bufs[i] = None;
                self.slots.dropped(i);
            }
            self.drawn_at = None;
        }
    }

    /// Do what the app asked for in a call.
    fn obey(&mut self, ctl: Control) {
        if ctl.redraw {
            self.redraw.request();
        }
        self.exit |= ctl.close;
        if let Some(c) = ctl.cursor {
            self.cursor = c;
            self.apply_cursor();
        }
        if let Some(data) = ctl.set_clipboard {
            self.copy(data);
        }
        if let Some(groups) = ctl.read_clipboard {
            self.paste(&groups);
        }
    }

    /// Become the owner of the selection, with `data`.
    fn copy(&mut self, data: Vec<(String, Arc<[u8]>)>) {
        let (Some(ddm), Some(device)) = (&self.ddm, &self.device) else { return };
        let own = offer_for(&data);
        if own.is_empty() {
            return;
        }
        let source = ddm.create_copy_paste_source(&self.qh, own.iter().map(|(m, _)| m));
        source.set_selection(device, self.serial);
        // The old source is destroyed here. Its `cancelled` event comes later, for a source that is no longer ours.
        self.source = Some(source);
        self.own = own;
    }

    /// Read the selection for `Control::read_clipboard`. The answer is one `Event::Clipboard`, queued in `pending`.
    fn paste(&mut self, groups: &[Vec<String>]) {
        let want: Vec<Vec<&str>> = groups.iter().map(|g| g.iter().map(String::as_str).collect()).collect();
        let want: Vec<&[&str]> = want.iter().map(Vec::as_slice).collect();
        self.reading = None;
        if self.source.is_some() {
            // Our own selection: no pipe, the data is here.
            let own: Vec<String> = self.own.iter().map(|(m, _)| m.clone()).collect();
            let mut job = ReadJob::new(&own, &want);
            while let Some(m) = job.next() {
                let bytes = self.own.iter().find(|(o, _)| *o == m).map(|(_, b)| b.to_vec()).unwrap_or_default();
                job.push(&m, bytes);
            }
            self.pending.push(Event::Clipboard { items: job.items() });
            return;
        }
        match self.device.as_ref().and_then(|d| d.data().selection_offer()) {
            Some(offer) => {
                let offered = offer.with_mime_types(|m| m.to_vec());
                self.reading = Some(Reading { job: ReadJob::new(&offered, &want), offer, cur: None });
                self.advance_read();
            }
            None => self.pending.push(Event::Clipboard { items: Vec::new() }),
        }
    }

    /// Start the read of the next type, or answer when all are read.
    fn advance_read(&mut self) {
        while let Some(r) = &mut self.reading {
            let Some(mime) = r.job.next() else {
                let r = self.reading.take().expect("checked above");
                self.pending.push(Event::Clipboard { items: r.job.items() });
                return;
            };
            // The request goes out with the next flush of the loop, before the poll.
            if let Ok(pipe) = r.offer.receive(mime.clone()) {
                r.cur = Some((mime, pipe, Vec::new()));
                return;
            }
        }
    }

    /// Whether a clipboard pipe is open.
    fn io_open(&self) -> bool {
        self.reading.as_ref().is_some_and(|r| r.cur.is_some()) || !self.writes.is_empty()
    }

    /// Add the open pipes to the poll set: the read pipe first, then the write pipes, in the order of `io_ready`.
    fn io_fds<'a>(&'a self, out: &mut Vec<PollFd<'a>>) {
        if let Some((_, pipe, _)) = self.reading.as_ref().and_then(|r| r.cur.as_ref()) {
            out.push(PollFd::new(pipe, PollFlags::IN));
        }
        for w in &self.writes {
            out.push(PollFd::new(&w.pipe, PollFlags::OUT));
        }
    }

    /// Do the reads and writes that `poll` said are ready. `ready` is in the order of `io_fds`.
    fn io_ready(&mut self, ready: &[PollFlags]) {
        let mut ready = ready.iter();
        if self.reading.as_ref().is_some_and(|r| r.cur.is_some()) {
            if !ready.next().expect("one entry for each pipe").is_empty() {
                self.read_chunk();
            }
        }
        self.writes.retain_mut(|w| {
            let r = ready.next().expect("one entry for each pipe");
            if r.is_empty() {
                return true;
            }
            if r.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) {
                return false;
            }
            let end = (w.done + CHUNK).min(w.data.len());
            match w.pipe.write(&w.data[w.done..end]) {
                Ok(n) => {
                    w.done += n;
                    w.done < w.data.len()
                }
                Err(e) => e.kind() == std::io::ErrorKind::Interrupted,
            }
        });
    }

    /// Read what the open pipe has now. The end of the pipe finishes the type.
    fn read_chunk(&mut self) {
        let Some(r) = &mut self.reading else { return };
        let Some((_, pipe, buf)) = &mut r.cur else { return };
        let mut chunk = [0u8; 65536];
        let n = match pipe.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return,
            Err(_) => 0,
        };
        buf.extend_from_slice(&chunk[..n]);
        let too_big = buf.len() > MAX_PASTE;
        if n == 0 || too_big {
            let (mime, _, bytes) = r.cur.take().expect("checked above");
            if !too_big {
                r.job.push(&mime, bytes);
            }
            self.advance_read();
        }
    }

    fn apply_cursor(&self) {
        let icon = match self.cursor {
            Cursor::Default => CursorIcon::Default,
            Cursor::Pointer => CursorIcon::Pointer,
            Cursor::Text => CursorIcon::Text,
        };
        // No enter serial yet (the pointer is not over the window) is not an error: the shape is set on the next enter.
        if let Some(p) = &self.pointer {
            let _ = p.set_cursor(&self.conn, icon);
        }
    }

    fn maybe_draw(&mut self, app: &mut dyn App, qh: &QueueHandle<State>) {
        if !self.redraw.should_draw(self.slots.any_free()) {
            return;
        }
        let size = self.want;
        if size != self.drawn {
            app.resize(size.0, size.1);
            self.drawn = size;
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
        // The program draws straight into the shared memory. The map starts on a page, so it holds whole `u32`s; a little-endian
        // `0xAARRGGBB` is the memory order of `wl_shm` XRGB8888.
        let n = size.0 as usize * size.1 as usize;
        let out = &mut as_pixels(buf.pool.mmap())[..n];
        let mut ctl = Control::default();
        if !app.frame(&mut ctl, out) {
            if self.shown == Some(size) {
                self.redraw.kept();
                self.exit |= ctl.close;
                return;
            }
            out.fill(0xffff_ffff);
        }
        let surface = self.window.wl_surface();
        surface.frame(qh, FrameCallbackData(surface.clone()));
        surface.attach(Some(&buf.buffer), 0, 0);
        surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        surface.commit();
        self.slots.attached(i);
        self.shown = Some(size);
        self.drawn_at = Some(Instant::now());
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

const _: () = assert!(cfg!(target_endian = "little"), "the pixels are written as native `u32`s into an XRGB8888 buffer");

/// The bytes of a shared-memory map as pixels.
fn as_pixels(map: &mut [u8]) -> &mut [u32] {
    // SAFETY: any 4 bytes are a valid `u32`, and `align_to_mut` gives only aligned whole values in the middle part.
    let (head, px, _) = unsafe { map.align_to_mut::<u32>() };
    assert!(head.is_empty(), "a shared-memory map starts on a page");
    px
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

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        // The data device is made with the first capability: `new_seat` does not come for a seat that the registry already had.
        if self.device.is_none() {
            self.device = self.ddm.as_ref().map(|m| m.get_data_device(qh, &seat));
        }
        match capability {
            Capability::Pointer if self.pointer.is_none() => {
                let surface = self.compositor.create_surface(qh);
                match self.seat.get_pointer_with_theme::<_, ()>(qh, &seat, self.shm.wl_shm(), surface, ThemeSpec::System) {
                    Ok(p) => self.pointer = Some(p),
                    Err(e) => self.error = Some(format!("pointer: {e}")),
                }
            }
            Capability::Keyboard if self.keyboard.is_none() => match self.seat.get_keyboard(qh, &seat, None) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => self.error = Some(format!("keyboard: {e}")),
            },
            _ => {}
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        match capability {
            Capability::Pointer => self.pointer = None,
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
                self.repeat.clear();
                self.repeating = None;
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        // The axis events of this frame add up to one wheel event: a notch can arrive as `value120`, `discrete` and `absolute`.
        let (mut h, mut v) = (AxisScroll::default(), AxisScroll::default());
        let mut wheel_at = None;
        for e in events {
            if &e.surface != self.window.wl_surface() {
                continue;
            }
            self.pos = e.position;
            let (x, y) = e.position;
            match e.kind {
                PointerEventKind::Enter { .. } => {
                    // The shape must be set again after each enter.
                    self.apply_cursor();
                    self.pending.push(Event::PointerMove { x, y, buttons: self.buttons, mods: self.mods });
                }
                PointerEventKind::Leave { .. } => self.pending.push(Event::PointerLeave),
                PointerEventKind::Motion { .. } => self.pending.push(Event::PointerMove { x, y, buttons: self.buttons, mods: self.mods }),
                PointerEventKind::Press { button, .. } | PointerEventKind::Release { button, .. } => {
                    let pressed = matches!(e.kind, PointerEventKind::Press { .. });
                    let Some(b) = input::button_of(button) else { continue };
                    let bit = input::button_bit(b);
                    if pressed {
                        self.buttons |= bit;
                    } else {
                        self.buttons &= !bit;
                    }
                    self.pending.push(Event::PointerButton { x, y, button: b, buttons: self.buttons, pressed, mods: self.mods });
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    for (sum, a) in [(&mut h, horizontal), (&mut v, vertical)] {
                        sum.absolute += a.absolute;
                        sum.discrete += a.discrete;
                        sum.value120 += a.value120;
                    }
                    wheel_at = Some((x, y));
                }
            }
        }
        if let Some((x, y)) = wheel_at {
            let (dx, dy) = (input::wheel_px(h.value120, h.discrete, h.absolute), input::wheel_px(v.value120, v.discrete, v.absolute));
            if dx != 0.0 || dy != 0.0 {
                self.pending.push(Event::Wheel { x, y, dx, dy, mods: self.mods });
            }
        }
    }
}

impl KeyboardHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.serial = serial;
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        // All held keys are released when the window loses the keyboard focus, and the repeat stops.
        self.repeat.clear();
        self.repeating = None;
        self.mods = Mods::default();
    }

    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, e: KeyEvent) {
        self.serial = serial;
        let ev = self.key_event(e.raw_code, e.keysym, e.utf8.clone(), true, false);
        if input::repeats(input::code_of(e.raw_code)) {
            self.repeat.press(e.raw_code, Instant::now());
            self.repeating = Some((e.raw_code, e.keysym, e.utf8));
        }
        self.pending.push(ev);
    }

    // Only for a compositor that repeats by itself (we turn that off: the loop repeats, one source for the rate).
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}

    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, e: KeyEvent) {
        self.repeat.release(e.raw_code);
        if self.repeating.as_ref().is_some_and(|(c, ..)| *c == e.raw_code) {
            self.repeating = None;
        }
        let ev = self.key_event(e.raw_code, e.keysym, None, false, false);
        self.pending.push(ev);
    }

    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = Mods { shift: m.shift, ctrl: m.ctrl, alt: m.alt, meta: m.logo };
    }

    fn update_repeat_info(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, info: RepeatInfo) {
        self.repeat = match info {
            RepeatInfo::Repeat { rate, delay } => Repeat::new(rate.get(), delay),
            RepeatInfo::Disable => Repeat::new(0, 0),
        };
    }
}

impl DataDeviceHandler for State {
    // Drag and drop is not here. The selection is read from the device when the app asks.
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for State {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, mime: String, pipe: WritePipe) {
        // A request for a source that is gone, or for a type that we do not have, gets a closed pipe: the reader sees no data.
        if self.source.as_ref().is_some_and(|s| s.inner() == source) {
            if let Some((_, data)) = self.own.iter().find(|(m, _)| *m == mime) {
                self.writes.push(Send { pipe, data: data.clone(), done: 0 });
            }
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        // Another program copied. A `cancelled` for an older source of ours changes nothing.
        if self.source.as_ref().is_some_and(|s| s.inner() == source) {
            self.source = None;
            self.own.clear();
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_dispatch2!(State);
delegate_registry!(State);
