//! A window run keeps no more memory than a headless run plus the window itself. The page of a window runs on its own thread, next to the
//! thread of the driver and the workers of the network, so with two glibc arenas the memory that one thread frees stays in its arena and
//! the next thread cannot use it. In 50 runs of the login journey in one process the peak RSS was 37 to 40 MiB in a window and 17 to 18 MiB
//! headless: the window itself is two buffers of 3.5 MiB and a canvas of 3.5 MiB, about 11 MiB, and the rest was free memory in arenas.
//! A window needs a Wayland compositor: the run is `cage` with the headless backend (it never touches the desktop of a person).

mod common;

use common::{EMAIL, PASSWORD};
use std::path::PathBuf;
use std::process::Command;

/// The peak RSS in KiB after the last of 50 runs of the staging journey on the fixture app, or `None` if `cage` is not installed.
fn peak_kib(window: bool) -> Option<u64> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = std::env::temp_dir().join(format!("bare-arenas-{window}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, format!("EMAIL={EMAIL}\nPASSWORD={PASSWORD}\n")).unwrap();
    let base = format!("BASE={}", common::serve_app());
    let steps = root.join("tools/drive/staging-journey.steps");
    let drive = ["--quiet", "--repeat", "50", "--secrets", secrets.to_str().unwrap(), "--var", &base, "--var", "QUERY=Beta", steps.to_str().unwrap()];
    let mut cmd;
    if window {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| std::env::temp_dir().into_os_string());
        cmd = Command::new("cage");
        cmd.arg("--").arg(env!("CARGO_BIN_EXE_bare-drive")).arg("--window");
        cmd.env_remove("WAYLAND_DISPLAY").env("XDG_RUNTIME_DIR", runtime).env("WLR_BACKENDS", "headless");
        cmd.env("WLR_LIBINPUT_NO_DEVICES", "1").env("WLR_RENDERER", "pixman");
    } else {
        cmd = Command::new(env!("CARGO_BIN_EXE_bare-drive"));
    }
    // A set `MALLOC_ARENA_MAX` would win over the choice of the program: the test checks that choice.
    let out = cmd.args(drive).env_remove("MALLOC_ARENA_MAX").output();
    let _ = std::fs::remove_dir_all(&dir);
    let out = match out {
        Ok(o) => o,
        Err(_) => return None,
    };
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.starts_with("{\"ok\":true"), "window {window}: the journey failed: {json:.300} {}", String::from_utf8_lossy(&out.stderr));
    json.rsplit("\"peak_rss_kib\":").next().and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
}

#[test]
fn a_window_run_holds_the_window_and_little_more_than_a_headless_run() {
    let Some(headless) = peak_kib(false) else { return };
    let Some(window) = peak_kib(true) else {
        eprintln!("cage is not installed: the window test did not run");
        return;
    };
    // The buffers and the canvas of the window: 11 MiB. 5 MiB more is room for the noise of the allocator.
    assert!(window < headless + (16 << 10), "peak RSS: {headless} KiB headless, {window} KiB in a window");
}
