//! The window on a real compositor. It opens a real window, so it runs only when asked: set `BARE_WINDOW_LIVE=1` and
//! point `WAYLAND_DISPLAY` at a compositor that is not a person's screen (a hyprcage screen, for example). With either
//! one missing, the test prints why and passes. `lifecycle.rs` checks the same rules against a compositor of our own.
#![cfg(target_os = "linux")]

use bare_window::{App, Config, Control, Window};

struct Count {
    frames: u32,
}

impl App for Count {
    fn resize(&mut self, _: u32, _: u32) {}
    fn frame(&mut self, ctl: &mut Control, out: &mut [u32]) -> bool {
        self.frames += 1;
        if self.frames == 3 {
            ctl.close();
        }
        out.fill(0xff30_6090);
        true
    }
}

#[test]
fn opens_draws_three_frames_and_closes() {
    if std::env::var_os("BARE_WINDOW_LIVE").is_none() || std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("skipped: set BARE_WINDOW_LIVE=1 and WAYLAND_DISPLAY (a screen that is not a person's) to open a real window");
        return;
    }
    let config = Config { title: "bare window test".into(), app_id: "bare-test".into(), size: (320, 240) };
    let window = Window::open(&config).expect("connect to the compositor");
    let waker = window.waker();
    // Two wakes from another thread, after the first frame.
    std::thread::spawn(move || {
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            waker.wake();
        }
    });
    let mut app = Count { frames: 0 };
    let stats = window.run(&mut app).expect("the window runs and closes");
    assert_eq!(stats.frames, 3);
    assert_eq!(app.frames, 3);
    assert!(stats.buffers_made <= 2);
}
