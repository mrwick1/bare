//! The clipboard on a real compositor, with `wl-copy` and `wl-paste` as the other program. It runs only when asked: set `BARE_WINDOW_LIVE=1`
//! and point `WAYLAND_DISPLAY` at a compositor that is not a person's screen (`WLR_BACKENDS=headless cage -- sleep 60`, for example). With
//! either one missing, or no `wl-copy`, the test prints why and passes.
#![cfg(target_os = "linux")]

use bare_window::{App, Config, Control, Event, Window, clipboard};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::io::Write;

/// 0: nothing yet. 1: the window copied. 2: `wl-copy` copied, the window may read. 3: the window read.
static PHASE: AtomicU8 = AtomicU8::new(0);

struct Clip {
    frames: u32,
    got: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
}

impl App for Clip {
    fn resize(&mut self, _: u32, _: u32) {}
    fn frame(&mut self, _: &mut Control, out: &mut [u32]) -> bool {
        self.frames += 1;
        out.fill(0xff30_6090);
        true
    }
    fn tick(&mut self, ctl: &mut Control) -> Option<std::time::Instant> {
        match PHASE.load(Ordering::SeqCst) {
            0 if self.frames > 0 => {
                // A big text and an image type: the pipe takes it in many writes.
                let big: Vec<u8> = "from bare ".bytes().cycle().take(300_000).collect();
                ctl.set_clipboard(vec![("text/plain".into(), big.into()), ("image/png".into(), vec![0x89, b'P', b'N', b'G'].into())]);
                PHASE.store(1, Ordering::SeqCst);
            }
            2 => {
                ctl.read_clipboard(&[clipboard::TEXT, clipboard::IMAGE]);
                PHASE.store(3, Ordering::SeqCst);
            }
            _ => {}
        }
        None
    }
    fn event(&mut self, e: Event, ctl: &mut Control) {
        if let Event::Clipboard { items } = e {
            *self.got.lock().unwrap() = items;
            ctl.close();
        }
    }
}

#[test]
fn copies_to_and_reads_from_another_program() {
    if std::env::var_os("BARE_WINDOW_LIVE").is_none() || std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipped: set BARE_WINDOW_LIVE=1 and WAYLAND_DISPLAY (a screen that is not a person's) to open a real window");
        return;
    }
    if Command::new("wl-copy").arg("--version").stdout(Stdio::null()).status().is_err() {
        eprintln!("skipped: no wl-copy");
        return;
    }
    let config = Config { title: "bare clipboard test".into(), app_id: "bare-test".into(), size: (200, 100) };
    let window = Window::open(&config).expect("connect to the compositor");
    let waker = window.waker();
    let pasted = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let seen = pasted.clone();
    std::thread::spawn(move || {
        // The window copies on its first tick after the first frame. Wake it until it did.
        while PHASE.load(Ordering::SeqCst) < 1 {
            waker.wake();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let text = Command::new("wl-paste").arg("-n").output().expect("wl-paste");
        let png = Command::new("wl-paste").args(["-n", "-t", "image/png"]).output().expect("wl-paste image");
        *seen.lock().unwrap() = vec![("text".to_string(), text.stdout), ("png".to_string(), png.stdout)];
        let mut copy = Command::new("wl-copy").arg("from wl-copy").stdin(Stdio::piped()).spawn().expect("wl-copy");
        copy.stdin.take().map(|mut s| s.flush());
        let _ = copy.wait();
        PHASE.store(2, Ordering::SeqCst);
        waker.wake();
    });
    let got = Arc::new(Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let mut app = Clip { frames: 0, got: got.clone() };
    window.run(&mut app).expect("run");
    let from_bare = pasted.lock().unwrap().clone();
    assert_eq!(from_bare[0].1.len(), 300_000, "wl-paste read all the text");
    assert!(from_bare[0].1.starts_with(b"from bare from bare "));
    assert_eq!(from_bare[1].1, [0x89, b'P', b'N', b'G']);
    let read = got.lock().unwrap().clone();
    assert_eq!(read, vec![("text/plain;charset=utf-8".to_string(), b"from wl-copy".to_vec())], "the window read what wl-copy set");
}
