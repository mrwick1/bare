//! A step that waits for a reply sees it as soon as it is in, in a window too. The page of a window asks for a call of `Page::tick`
//! only each second (its loop is woken when a reply ends), so a driver that slept until that time waited up to 1 s for each reply.
//! Each step also counts its ticks.

use bare_drive::run::{Options, Redactor, Report, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

const INDEX: &str = r#"<!doctype html><html><body><p id="n">0</p><script>addEventListener('load', function () { fetch('/slow').then(function (r) { return r.text() }).then(function (t) { document.getElementById('n').textContent = t }) })</script></body></html>"#;

/// `/slow` answers `done` after 150 ms; any other path gets the page.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let (mut seen, mut piece) = (Vec::new(), [0u8; 4096]);
                loop {
                    let end = loop {
                        if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                        match s.read(&mut piece) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&piece[..n]),
                        }
                    };
                    let slow = seen.starts_with(b"GET /slow ");
                    seen.drain(..end);
                    let (kind, body) = if slow { ("text/plain", "done") } else { ("text/html", INDEX) };
                    if slow {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    let r = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                    if s.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn journey(window: bool) -> Report {
    let origin = serve();
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let src = "open ${ORIGIN}/\nexpect js \"document.getElementById('n').textContent === 'done'\"\n";
            let steps = parse(src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window, progress: false };
            run(&steps, &opt, &mut Redactor::default())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn check(rep: &Report) {
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    let wait = &rep.steps[1];
    assert!(wait.ms < 600.0, "the step waited {:.0} ms for a reply of 150 ms", wait.ms);
    // Each step has ticks, the longest is not longer than the step, and the totals hold all the steps.
    for s in &rep.steps {
        assert!(s.ticks.n >= 1 && s.ticks.max.as_secs_f64() * 1000.0 <= s.ms + 1.0, "line {}: {:?} in {:.1} ms", s.line, s.ticks, s.ms);
    }
    assert_eq!(rep.ticks.n, rep.steps.iter().map(|s| s.ticks.n).sum::<u32>());
    assert!(rep.json(&Redactor::default()).contains("\"ticks_over_50\":"));
}

#[test]
fn a_headless_step_sees_a_reply_at_once() {
    check(&journey(false));
}

/// A window needs a Wayland compositor: the test runs itself again inside `cage` with the headless backend (it never touches the
/// desktop of a person), and the inner run writes `ok` to the file that the outer run names.
#[test]
fn a_window_step_sees_a_reply_at_once() {
    let name = "a_window_step_sees_a_reply_at_once";
    if let Some(done) = std::env::var_os("BARE_CAGE_DONE") {
        check(&journey(true));
        std::fs::write(done, "ok").unwrap();
        return;
    }
    let done = std::env::temp_dir().join(format!("bare-cage-wait-{}", std::process::id()));
    let _ = std::fs::remove_file(&done);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    let status = std::process::Command::new("cage")
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .env("WLR_RENDERER", "pixman")
        .env("BARE_CAGE_DONE", &done)
        .status();
    let Ok(status) = status else {
        eprintln!("cage is not installed: the window test did not run");
        return;
    };
    let result = std::fs::read_to_string(&done).ok();
    let _ = std::fs::remove_file(&done);
    assert!(result.as_deref() == Some("ok"), "the run in cage failed ({status})");
}
