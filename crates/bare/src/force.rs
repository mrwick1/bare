//! Counters for the passes that style and lay out a page, by the caller that forced them (`docs/progress.md`, "Who forces style and layout").
//! Off unless `BARE_FORCE_STATS` is set. Then each pass writes one line to stderr, and a script adds them up:
//! `FS style <hit|restyle|compute|plan|rules> <caller> <elements> <instructions>`,
//! `FS layout <hit|full|kept> <caller> <boxes> <instructions>` and `FS frame <caller> <1 when the call built a frame>`.
//! A caller is a `file:line` of a window frame request (`#[track_caller]`), or the name of a script API (`tag`). Instructions are the user
//! instructions of the thread (`perf_event_open`, Linux x86-64 only; 0 elsewhere).

use std::{cell::Cell, panic::Location, sync::OnceLock};

/// Is the counting on? One read of the environment.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BARE_FORCE_STATS").is_some())
}

type Caller = (&'static str, u32);

thread_local! {
    static CALLER: Cell<Caller> = const { Cell::new(("other", 0)) };
}

/// The caller of the passes that run while it lives. The outermost wins: a tag inside another does nothing. Nothing happens when the
/// counting is off.
pub struct Tag(Option<Caller>);

impl Drop for Tag {
    fn drop(&mut self) {
        if let Some(c) = self.0 {
            CALLER.set(c);
        }
    }
}

/// A script API (`getComputedStyle`, `offsetWidth`) is the caller.
pub fn tag(name: &'static str) -> Tag {
    set(("other", 0) == CALLER.get(), (name, 0))
}

/// A frame request at `at` is the caller.
pub fn tag_at(at: &'static Location<'static>) -> Tag {
    set(("other", 0) == CALLER.get(), (at.file(), at.line()))
}

fn set(free: bool, caller: Caller) -> Tag {
    Tag((on() && free).then(|| CALLER.replace(caller)))
}

/// A frame request: did it build a frame?
pub fn frame(built: bool) {
    if on() {
        eprintln!("FS frame {} {}", caller(), u8::from(built));
    }
}

fn caller() -> String {
    let (name, line) = CALLER.get();
    if line == 0 { name.to_string() } else { format!("{name}:{line}") }
}

/// The instruction count at the start of a pass, `None` when the counting is off.
pub fn start() -> Option<u64> {
    on().then(instructions)
}

/// A style pass that began at `from` ended: `elements` were styled.
pub fn style(outcome: &str, from: Option<u64>, elements: u64) {
    done("style", outcome, from, elements);
}

/// A layout pass that began at `from` ended: `boxes` were laid out.
pub fn layout(outcome: &str, from: Option<u64>, boxes: u64) {
    done("layout", outcome, from, boxes);
}

fn done(kind: &str, outcome: &str, from: Option<u64>, n: u64) {
    if let Some(from) = from {
        eprintln!("FS {kind} {outcome} {} {n} {}", caller(), instructions().saturating_sub(from));
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn instructions() -> u64 {
    thread_local! {
        static FD: Cell<i32> = const { Cell::new(-2) };
    }
    unsafe extern "C" {
        fn syscall(n: i64, ...) -> i64;
        fn read(fd: i32, buf: *mut u8, n: usize) -> isize;
    }
    if FD.get() == -2 {
        // `perf_event_attr` (112 bytes): type 0 (hardware), size, config 1 (instructions), flags at word 5: exclude_kernel and exclude_hv.
        let mut attr = [0u64; 14];
        attr[0] = 112 << 32;
        attr[1] = 1;
        attr[5] = (1 << 5) | (1 << 6);
        // SAFETY: `attr` is a valid `perf_event_attr` of the size that it says; the call only reads it.
        FD.set(unsafe { syscall(298, attr.as_ptr(), 0i32, -1i32, -1i32, 0u64) } as i32);
    }
    let mut v = [0u8; 8];
    // SAFETY: reads at most 8 bytes into `v`.
    let ok = FD.get() >= 0 && unsafe { read(FD.get(), v.as_mut_ptr(), 8) } == 8;
    if ok { u64::from_ne_bytes(v) } else { 0 }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn instructions() -> u64 {
    0
}
