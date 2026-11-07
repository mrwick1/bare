//! The window against a compositor of our own, in the same process (no desktop, no window on a screen).
//!
//! The fake compositor is a few hundred lines on `wayland-server`. It keeps what a real one would keep that matters
//! here: which buffer is held until `release`, when the frame callback fires, and the memory of every buffer. It checks
//! that the program never attaches a held buffer and never writes into one (it hashes each held buffer at each turn).
//!
//! One test runs all the scenarios in turn, because it counts the open file descriptors of the process at the end.

use bare_window::{App, Config, Control, Stats, Waker, Window};
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use wayland_server::protocol::{wl_buffer, wl_callback, wl_compositor, wl_shm, wl_shm_pool, wl_surface};
use wayland_server::{Client, DataInit, Dispatch, Display, GlobalDispatch, New, Resource, WEnum};

// ---------------------------------------------------------------- the fake compositor

struct Commit {
    size: (i32, i32),
    /// The first pixel of the buffer. The test app writes the number of the frame in it.
    first_pixel: u32,
}

struct PoolData {
    map: memmap2::Mmap,
}

struct BufData {
    pool: Arc<PoolData>,
    offset: usize,
    size: (i32, i32),
}

impl BufData {
    fn bytes(&self) -> &[u8] {
        &self.pool.map[self.offset..self.offset + self.size.0 as usize * self.size.1 as usize * 4]
    }
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.chunks(8).fold(0xcbf29ce484222325u64, |h, c| {
        let mut w = [0u8; 8];
        w[..c.len()].copy_from_slice(c);
        (h ^ u64::from_le_bytes(w)).wrapping_mul(0x100000001b3)
    })
}

#[derive(Default)]
struct Server {
    commits: Vec<Commit>,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
    configured: bool,
    serial: u32,
    pending_buffer: Option<wl_buffer::WlBuffer>,
    callbacks: Vec<wl_callback::WlCallback>,
    /// The buffer that the last commit attached: the one on screen.
    current: Option<wl_buffer::WlBuffer>,
    /// Held buffers, with the hash of their memory when they were attached.
    held: HashMap<ObjectId, (wl_buffer::WlBuffer, u64)>,
    /// Release the replaced buffer at each commit. Off: hold every buffer until `release_all`.
    auto_release: bool,
    /// Fire the frame callbacks at each commit. Off: fire them with `fire_frames`.
    auto_frame: bool,
    live_pools: usize,
    live_buffers: usize,
    max_live_buffers: usize,
    violations: Vec<String>,
}

impl Server {
    fn check_held(&mut self) {
        for (buffer, h) in self.held.values() {
            let data = buffer.data::<BufData>().expect("buffer data");
            if hash(data.bytes()) != *h {
                self.violations.push("the program wrote into a buffer that the compositor holds".into());
            }
        }
    }

    fn fire_frames(&mut self) {
        for cb in self.callbacks.drain(..) {
            cb.done(0);
        }
    }

    fn release_all(&mut self) {
        for (_, (buffer, _)) in self.held.drain() {
            buffer.release();
        }
        self.current = None;
    }

    fn configure(&mut self, w: i32, h: i32) {
        let (top, surf) = (self.toplevel.as_ref().expect("toplevel"), self.xdg_surface.as_ref().expect("xdg_surface"));
        top.configure(w, h, vec![]);
        self.serial += 1;
        surf.configure(self.serial);
    }
}

struct Nobody;
impl ClientData for Nobody {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for Server {
    fn bind(_: &mut Self, _: &wayland_server::DisplayHandle, _: &Client, new: New<wl_compositor::WlCompositor>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(new, ());
    }
}
impl Dispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_compositor::WlCompositor, request: wl_compositor::Request, _: &(), _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_compositor::Request::CreateSurface { id } = request {
            init.init(id, ());
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Server {
    fn request(s: &mut Self, _: &Client, _: &wl_surface::WlSurface, request: wl_surface::Request, _: &(), _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_surface::Request::Attach { buffer, .. } => s.pending_buffer = buffer,
            wl_surface::Request::Frame { callback } => s.callbacks.push(init.init(callback, ())),
            wl_surface::Request::Commit => {
                s.check_held();
                let Some(buffer) = s.pending_buffer.take() else {
                    // The first commit has no buffer. Send the first configure (size 0 by 0: the program chooses).
                    if !s.configured {
                        s.configured = true;
                        s.configure(0, 0);
                    }
                    return;
                };
                let data = buffer.data::<BufData>().expect("buffer data");
                if s.held.contains_key(&buffer.id()) {
                    s.violations.push("the program attached a buffer that the compositor holds".into());
                }
                let first_pixel = u32::from_le_bytes(data.bytes()[..4].try_into().unwrap());
                s.commits.push(Commit { size: data.size, first_pixel });
                s.held.insert(buffer.id(), (buffer.clone(), hash(data.bytes())));
                // The buffer that this one replaces is no longer shown.
                if let Some(old) = s.current.replace(buffer.clone())
                    && s.auto_release
                    && old.id() != buffer.id()
                    && let Some((old, _)) = s.held.remove(&old.id())
                {
                    old.release();
                }
                if s.auto_frame {
                    s.fire_frames();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_callback::WlCallback, _: wl_callback::Request, _: &(), _: &wayland_server::DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl GlobalDispatch<wl_shm::WlShm, ()> for Server {
    fn bind(_: &mut Self, _: &wayland_server::DisplayHandle, _: &Client, new: New<wl_shm::WlShm>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(new, ()).format(wl_shm::Format::Xrgb8888);
    }
}
impl Dispatch<wl_shm::WlShm, ()> for Server {
    fn request(s: &mut Self, _: &Client, _: &wl_shm::WlShm, request: wl_shm::Request, _: &(), _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_shm::Request::CreatePool { id, fd, size } = request {
            let fd: OwnedFd = fd;
            let file = File::from(fd);
            // SAFETY: the program does not shrink the file, and the test only reads it.
            let map = unsafe { memmap2::MmapOptions::new().len(size as usize).map(&file) }.expect("map the pool");
            s.live_pools += 1;
            init.init(id, Arc::new(PoolData { map }));
        }
    }
}
impl Dispatch<wl_shm_pool::WlShmPool, Arc<PoolData>> for Server {
    fn request(s: &mut Self, _: &Client, _: &wl_shm_pool::WlShmPool, request: wl_shm_pool::Request, pool: &Arc<PoolData>, _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, format } = request {
            assert_eq!(stride, width * 4);
            assert!(matches!(format, WEnum::Value(wl_shm::Format::Xrgb8888)));
            s.live_buffers += 1;
            s.max_live_buffers = s.max_live_buffers.max(s.live_buffers);
            init.init(id, BufData { pool: pool.clone(), offset: offset as usize, size: (width, height) });
        }
    }
    fn destroyed(s: &mut Self, _: ClientId, _: &wl_shm_pool::WlShmPool, _: &Arc<PoolData>) {
        s.live_pools -= 1;
    }
}
impl Dispatch<wl_buffer::WlBuffer, BufData> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_buffer::WlBuffer, _: wl_buffer::Request, _: &BufData, _: &wayland_server::DisplayHandle, _: &mut DataInit<'_, Self>) {}
    fn destroyed(s: &mut Self, _: ClientId, buffer: &wl_buffer::WlBuffer, _: &BufData) {
        let id = buffer.id();
        s.live_buffers -= 1;
        // A held buffer that is destroyed is no longer held. (A real compositor keeps its own reference.)
        s.held.remove(&id);
        if s.current.as_ref().is_some_and(|b| b.id() == id) {
            s.current = None;
        }
    }
}

impl GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for Server {
    fn bind(_: &mut Self, _: &wayland_server::DisplayHandle, _: &Client, new: New<xdg_wm_base::XdgWmBase>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(new, ());
    }
}
impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Server {
    fn request(s: &mut Self, _: &Client, _: &xdg_wm_base::XdgWmBase, request: xdg_wm_base::Request, _: &(), _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let xdg_wm_base::Request::GetXdgSurface { id, .. } = request {
            s.xdg_surface = Some(init.init(id, ()));
        }
    }
}
impl Dispatch<xdg_surface::XdgSurface, ()> for Server {
    fn request(s: &mut Self, _: &Client, _: &xdg_surface::XdgSurface, request: xdg_surface::Request, _: &(), _: &wayland_server::DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let xdg_surface::Request::GetToplevel { id } = request {
            s.toplevel = Some(init.init(id, ()));
        }
    }
}
impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &xdg_toplevel::XdgToplevel, _: xdg_toplevel::Request, _: &(), _: &wayland_server::DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

struct Compositor {
    display: Display<Server>,
    server: Server,
}

impl Compositor {
    fn new() -> (Compositor, UnixStream) {
        let display = Display::<Server>::new().expect("display");
        let mut dh = display.handle();
        dh.create_global::<Server, wl_compositor::WlCompositor, ()>(6, ());
        dh.create_global::<Server, wl_shm::WlShm, ()>(1, ());
        dh.create_global::<Server, xdg_wm_base::XdgWmBase, ()>(6, ());
        let (ours, theirs) = UnixStream::pair().expect("socket pair");
        dh.insert_client(ours, Arc::new(Nobody)).expect("insert client");
        let server = Server { auto_release: true, auto_frame: true, ..Server::default() };
        (Compositor { display, server }, theirs)
    }

    /// Serve the program until `done` is true. Panics after 5 seconds.
    fn pump_until(&mut self, what: &str, done: impl Fn(&Server) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(&self.server) {
            assert!(Instant::now() < end, "timeout: {what}");
            self.turn(Duration::from_millis(5));
        }
        assert!(self.server.violations.is_empty(), "{:?}", self.server.violations);
    }

    /// Serve for a time, doing nothing else.
    fn pump_for(&mut self, time: Duration) {
        let end = Instant::now() + time;
        while Instant::now() < end {
            self.turn(Duration::from_millis(5));
        }
        assert!(self.server.violations.is_empty(), "{:?}", self.server.violations);
    }

    fn turn(&mut self, wait: Duration) {
        // Send what the test queued (a configure, a release) before we sleep.
        self.display.flush_clients().expect("flush");
        let fd = self.display.backend().poll_fd().try_clone_to_owned().expect("fd");
        let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        let t = rustix::fs::Timespec { tv_sec: 0, tv_nsec: wait.as_nanos() as _ };
        let _ = rustix::event::poll(&mut fds, Some(&t));
        self.display.dispatch_clients(&mut self.server).expect("dispatch");
        self.server.check_held();
        self.display.flush_clients().expect("flush");
    }
}

// ---------------------------------------------------------------- the program

struct TestApp {
    size: (u32, u32),
    frames: Arc<AtomicU64>,
    /// Frames after this one keep the frame on screen (`App::frame` gives `false`).
    keep_after: Arc<AtomicU64>,
    close_after: u64,
    sizes: Arc<Mutex<Vec<(u32, u32)>>>,
    /// Timers: the times of the work that `tick` gives, and when each one ran.
    due: Vec<Instant>,
    ran: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

impl TestApp {
    fn new(frames: &Arc<AtomicU64>, sizes: &Arc<Mutex<Vec<(u32, u32)>>>) -> TestApp {
        TestApp { size: (0, 0), frames: frames.clone(), keep_after: Arc::new(AtomicU64::new(u64::MAX)), close_after: u64::MAX, sizes: sizes.clone(), due: vec![], ran: Arc::default() }
    }
}

impl App for TestApp {
    fn resize(&mut self, w: u32, h: u32) {
        self.size = (w, h);
        self.sizes.lock().unwrap().push((w, h));
    }
    fn frame(&mut self, ctl: &mut Control, out: &mut [u32]) -> bool {
        let n = self.frames.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(out.len(), self.size.0 as usize * self.size.1 as usize);
        if n >= self.close_after {
            ctl.close();
        }
        if n > self.keep_after.load(Ordering::SeqCst) {
            return false;
        }
        // The number of the frame goes in the first pixel, so that the compositor can tell which frame it holds.
        out.fill(0xff20_4060);
        out[0] = 0xff00_0000 | n as u32;
        true
    }
    fn tick(&mut self, ctl: &mut Control) -> Option<Instant> {
        let now = Instant::now();
        while self.due.first().is_some_and(|&t| t <= now) {
            self.ran.lock().unwrap().push((self.due.remove(0), now));
            ctl.redraw();
        }
        self.due.first().copied()
    }
}

struct Run {
    handle: std::thread::JoinHandle<Result<Stats, bare_window::Error>>,
    waker: Waker,
    frames: Arc<AtomicU64>,
    keep_after: Arc<AtomicU64>,
    sizes: Arc<Mutex<Vec<(u32, u32)>>>,
    ran: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

fn start(comp: &mut Compositor, socket: UnixStream, close_after: u64) -> Run {
    start_with(comp, socket, close_after, vec![])
}

/// Start the program with timers at these times from now.
fn start_with(comp: &mut Compositor, socket: UnixStream, close_after: u64, timers: Vec<Duration>) -> Run {
    let frames = Arc::new(AtomicU64::new(0));
    let sizes = Arc::new(Mutex::new(vec![]));
    let ran = Arc::new(Mutex::new(vec![]));
    let keep_after = Arc::new(AtomicU64::new(u64::MAX));
    let (f, s, r, k) = (frames.clone(), sizes.clone(), ran.clone(), keep_after.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let config = Config { title: "test".into(), app_id: "bare-test".into(), size: (320, 240) };
        let window = Window::open_on(&config, socket)?;
        tx.send(window.waker()).unwrap();
        let mut app = TestApp::new(&f, &s);
        app.close_after = close_after;
        let now = Instant::now();
        app.due = timers.iter().map(|&d| now + d).collect();
        app.ran = r;
        app.keep_after = k;
        window.run(&mut app)
    });
    // Opening the window asks the compositor some questions: serve them while we wait.
    let end = Instant::now() + Duration::from_secs(5);
    let waker = loop {
        comp.turn(Duration::from_millis(5));
        match rx.try_recv() {
            Ok(waker) => break waker,
            Err(_) => assert!(Instant::now() < end, "the window did not open"),
        }
    };
    Run { handle, waker, frames, keep_after, sizes, ran }
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    s.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches(" kB").parse().unwrap()
}

// ---------------------------------------------------------------- the scenarios

#[test]
fn window_lifecycle() {
    let fds = open_fds();
    first_frame_and_idle();
    a_frame_waits_for_the_frame_callback();
    resize_50_times_and_redraw();
    held_buffers_are_never_touched();
    the_program_closes_the_window();
    the_compositor_closes_the_window();
    work_of_the_app_wakes_the_loop();
    a_kept_frame_commits_nothing();
    an_idle_window_drops_its_free_buffer();
    assert_eq!(open_fds(), fds, "file descriptors were left open");
}

fn first_frame_and_idle() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame at the size of the program", |s| s.commits.len() == 1);
    // The compositor left the size to the program: 320 by 240.
    assert_eq!(comp.server.commits[0].size, (320, 240));
    assert_eq!(comp.server.commits[0].first_pixel, 0xff00_0001);
    // The same size again (a focus change in a real compositor) changes nothing. An idle window draws nothing.
    comp.server.configure(320, 240);
    comp.pump_for(Duration::from_millis(300));
    assert_eq!(comp.server.commits.len(), 1);
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    assert_eq!(stats.frames, 1);
    assert_eq!(stats.buffers_made, 1);
    assert_eq!(stats.configures, 2);
    // A busy loop would count thousands in these 300 ms.
    assert!(stats.wakeups < 10, "{stats:?}");
}

/// The loop wakes for the work that `App::tick` gives (page timers between input events), on time, and only then: after the last
/// timer it sleeps with no timeout again.
fn work_of_the_app_wakes_the_loop() {
    let (mut comp, socket) = Compositor::new();
    let ms = Duration::from_millis;
    let run = start_with(&mut comp, socket, u64::MAX, vec![ms(60), ms(120), ms(180)]);
    comp.pump_until("first frame", |s| !s.commits.is_empty());
    comp.pump_until("a frame for each timer", |s| s.commits.len() == 4);
    comp.pump_for(ms(300));
    assert_eq!(comp.server.commits.len(), 4, "no frame after the last timer");
    let ran = run.ran.lock().unwrap().clone();
    assert_eq!(ran.len(), 3);
    for (due, at) in &ran {
        // On time: not early, and late only by the time of a scheduler turn.
        assert!(at >= due && *at - *due < ms(40), "a timer ran {:?} late", *at - *due);
    }
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    // The first frame, three timers, and the close: a loop that polled would count hundreds in these 500 ms.
    assert!(stats.wakeups < 20, "{stats:?}");
}

/// An idle window keeps only the buffer that the compositor shows: the free one goes after `IDLE` with no frame. The next frame makes it again.
fn an_idle_window_drops_its_free_buffer() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame", |s| s.commits.len() == 1);
    run.waker.wake();
    comp.pump_until("second frame", |s| s.commits.len() == 2);
    assert_eq!(comp.server.live_buffers, 2);
    comp.pump_for(Duration::from_millis(300));
    assert_eq!(comp.server.live_buffers, 2, "a short pause keeps both buffers");
    comp.pump_until("the free buffer is dropped", |s| s.live_buffers == 1);
    assert!(comp.server.current.is_some(), "the buffer on screen stays");
    run.waker.wake();
    comp.pump_until("a frame after the pause", |s| s.commits.len() == 3);
    assert_eq!(comp.server.commits[2].first_pixel, 0xff00_0003);
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    assert_eq!(stats.buffers_made, 3);
    // Two frames, one drop, one frame and the close: the drop is one timer, not a poll.
    assert!(stats.wakeups < 20, "{stats:?}");
}

/// A program that keeps its frame (a page that waits for its next document) commits nothing: the compositor shows the last frame. With
/// nothing of this size on screen, the window shows white.
fn a_kept_frame_commits_nothing() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, u64::MAX);
    run.keep_after.store(0, Ordering::SeqCst);
    comp.pump_until("a first frame, white", |s| s.commits.len() == 1);
    assert_eq!(comp.server.commits[0].first_pixel, 0xffff_ffff);
    run.keep_after.store(u64::MAX, Ordering::SeqCst);
    run.waker.wake();
    comp.pump_until("a drawn frame", |s| s.commits.len() == 2);
    assert_eq!(comp.server.commits[1].first_pixel, 0xff00_0002);
    run.keep_after.store(2, Ordering::SeqCst);
    run.waker.wake();
    comp.pump_until("the program was asked", |_| run.frames.load(Ordering::SeqCst) == 3);
    comp.pump_for(Duration::from_millis(100));
    assert_eq!(comp.server.commits.len(), 2, "a kept frame commits nothing");
    run.keep_after.store(u64::MAX, Ordering::SeqCst);
    run.waker.wake();
    comp.pump_until("the next drawn frame", |s| s.commits.len() == 3);
    assert_eq!(comp.server.commits[2].first_pixel, 0xff00_0004);
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    assert_eq!(stats.frames, 3);
}

fn a_frame_waits_for_the_frame_callback() {
    let (mut comp, socket) = Compositor::new();
    comp.server.auto_frame = false;
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame", |s| s.commits.len() == 1);
    comp.server.configure(400, 300);
    comp.pump_for(Duration::from_millis(100));
    assert_eq!(comp.server.commits.len(), 1, "the last frame was not shown yet: no new frame");
    comp.server.fire_frames();
    comp.pump_until("frame at the new size", |s| s.commits.len() == 2);
    assert_eq!(comp.server.commits[1].size, (400, 300));
    // Many configures that come before the next callback make one frame.
    for w in [410, 420, 430, 440, 450] {
        comp.server.configure(w, 300);
    }
    comp.pump_for(Duration::from_millis(50));
    comp.server.fire_frames();
    comp.pump_until("one frame at the last size", |s| s.commits.len() == 3);
    comp.server.fire_frames();
    comp.pump_for(Duration::from_millis(50));
    assert_eq!(comp.server.commits.len(), 3);
    assert_eq!(comp.server.commits[2].size, (450, 300));
    comp.server.toplevel.as_ref().unwrap().close();
    finish(&mut comp, run);
}

fn resize_50_times_and_redraw() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame", |s| s.commits.len() == 1);
    let size = |i: u32| (200 + (i * 37) % 400, 150 + (i * 53) % 300);
    let mut latency = vec![];
    let mut rss_after_warmup = 0;
    for i in 0..50u32 {
        let (w, h) = size(i);
        let n = comp.server.commits.len();
        let t = Instant::now();
        comp.server.configure(w as i32, h as i32);
        comp.pump_until("frame at the new size", |s| s.commits.len() == n + 1);
        assert_eq!(comp.server.commits[n].size, (w as i32, h as i32));
        latency.push(t.elapsed());
        assert!(comp.server.live_buffers <= 3, "{} buffers alive", comp.server.live_buffers);
        if i == 9 {
            rss_after_warmup = rss_kb();
        }
    }
    let growth = rss_kb() as i64 - rss_after_warmup as i64;
    latency.sort();
    eprintln!(
        "50 resizes: configure to commit, median {:?}, max {:?}; RSS change over the last 40: {growth} kB; most buffers alive {}",
        latency[25], latency[49], comp.server.max_live_buffers
    );
    assert!(growth < 4096, "RSS grew by {growth} kB over 40 resizes");
    // A window that keeps its size makes no new buffers: 30 redraws from another thread.
    let size = size(49);
    let made = comp.server.max_live_buffers;
    for _ in 0..30 {
        let n = comp.server.commits.len();
        run.waker.wake();
        comp.pump_until("redraw after a wake", |s| s.commits.len() == n + 1);
        assert_eq!(comp.server.commits.last().unwrap().size, (size.0 as i32, size.1 as i32));
    }
    assert_eq!(comp.server.live_buffers, 2);
    assert_eq!(comp.server.max_live_buffers, made);
    // The number of frame in the first pixel grows by one at each commit.
    let pixels: Vec<u32> = comp.server.commits.iter().map(|c| c.first_pixel & 0xffffff).collect();
    assert!(pixels.windows(2).all(|p| p[1] == p[0] + 1), "{pixels:?}");
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    assert!(stats.buffers_made <= 2 + 50, "{stats:?}");
    // The program made a canvas for each new size only, never for a redraw.
    assert_eq!(stats.frames as usize, comp.server.commits.len());
}

fn held_buffers_are_never_touched() {
    let (mut comp, socket) = Compositor::new();
    comp.server.auto_release = false;
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame", |s| s.commits.len() == 1);
    run.waker.wake();
    comp.pump_until("second frame", |s| s.commits.len() == 2);
    // Both buffers are held. A redraw has no buffer: nothing is drawn, and nothing is written.
    for _ in 0..5 {
        run.waker.wake();
    }
    comp.pump_for(Duration::from_millis(100));
    assert_eq!(comp.server.commits.len(), 2);
    assert_eq!(run.frames.load(Ordering::SeqCst), 2, "the program was not even asked for a picture");
    // A resize waits too: the old picture stays.
    comp.server.configure(500, 200);
    comp.pump_for(Duration::from_millis(100));
    assert_eq!(comp.server.commits.len(), 2);
    comp.server.release_all();
    comp.pump_until("frame after the release", |s| s.commits.len() == 3);
    assert_eq!(comp.server.commits[2].size, (500, 200));
    assert_eq!(run.sizes.lock().unwrap().last(), Some(&(500, 200)));
    assert!(comp.server.max_live_buffers <= 3);
    comp.server.toplevel.as_ref().unwrap().close();
    finish(&mut comp, run);
}

fn the_program_closes_the_window() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, 3);
    comp.pump_until("three frames", |s| s.commits.len() == 1);
    for _ in 0..2 {
        run.waker.wake();
        comp.pump_for(Duration::from_millis(30));
    }
    let stats = finish(&mut comp, run);
    assert_eq!(stats.frames, 3);
    // The program's buffers and pools are gone when it returns.
    assert_eq!((comp.server.live_buffers, comp.server.live_pools), (0, 0));
}

fn the_compositor_closes_the_window() {
    let (mut comp, socket) = Compositor::new();
    let run = start(&mut comp, socket, u64::MAX);
    comp.pump_until("first frame", |s| s.commits.len() == 1);
    comp.server.toplevel.as_ref().unwrap().close();
    let stats = finish(&mut comp, run);
    assert_eq!(stats.frames, 1);
    assert_eq!((comp.server.live_buffers, comp.server.live_pools), (0, 0));
}

/// Serve until the program returns, and give back its numbers.
fn finish(comp: &mut Compositor, run: Run) -> Stats {
    let end = Instant::now() + Duration::from_secs(5);
    while !run.handle.is_finished() {
        assert!(Instant::now() < end, "the window did not close");
        comp.turn(Duration::from_millis(5));
    }
    let stats = run.handle.join().expect("the window thread did not panic").expect("the window ran");
    comp.pump_for(Duration::from_millis(10));
    stats
}
