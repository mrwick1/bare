//! `bare-drive --profile`: a login in one process, then a new process that opens the same profile, sends the cookie on its first
//! request and sees the `localStorage` item. A held profile `default` gives a run in memory. The server is a loopback server of
//! this test.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<(String, String)>>>;

fn serve() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Default::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || connection(s, log));
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn connection(mut s: TcpStream, log: Seen) {
    let (mut buf, mut piece) = (Vec::new(), [0u8; 4096]);
    loop {
        let end = loop {
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        buf.drain(..end);
        let path = head.split(' ').nth(1).unwrap_or("/").to_string();
        let cookie = head.lines().find_map(|l| l.strip_prefix("Cookie: ")).unwrap_or("none").to_string();
        log.lock().unwrap().push((path.clone(), cookie.clone()));
        let (extra, body) = if path == "/login" {
            // A session cookie (no expiry): the profile keeps it, as Chrome does for a session restore.
            ("Set-Cookie: sid=abc-secret-1; Path=/; HttpOnly\r\n", "<p>in</p><script>localStorage.setItem('a','ls-secret-2');sessionStorage.setItem('t','x')</script>".to_string())
        } else {
            ("", format!("<p id=c>cookie: {cookie}</p>"))
        };
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bare-drive-profile-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Run `bare-drive` on `steps` with `args`; the data directory is `data`.
fn drive(dir: &Path, data: &Path, base: &str, steps: &str, args: &[&str]) -> Output {
    let file = dir.join("j.steps");
    std::fs::write(&file, steps).unwrap();
    Command::new(env!("CARGO_BIN_EXE_bare-drive")).env("BARE_DATA_DIR", data).args(["--quiet", "--size", "300x120", "--var", &format!("URL={base}")]).args(args).arg(&file).output().unwrap()
}

#[test]
fn a_new_process_opens_the_profile_logged_in() {
    let (base, seen) = serve();
    let dir = temp("login");
    let profile = dir.join("p");
    let p = profile.to_str().unwrap();
    let out = drive(&dir, &dir.join("data"), &base, "open ${URL}/login\nexpect cookie sid\nexpect js \"localStorage.a.startsWith('ls-') && localStorage.a.length===11\"\n", &["--profile", p]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |f: &str| std::fs::metadata(profile.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(""), mode("cookies.bin"), mode("local.bin")), (0o700, 0o600, 0o600));
    }
    let steps = "open ${URL}/second\nexpect cookie sid\nexpect js \"localStorage.a.startsWith('ls-') && localStorage.a.length===11\"\nexpect js \"sessionStorage.length===0\"\n";
    let out2 = drive(&dir, &dir.join("data"), &base, steps, &["--profile", p]);
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stdout));
    let first = seen.lock().unwrap().iter().find(|(p, _)| p == "/second").cloned().unwrap();
    assert!(first.1.contains("sid=abc-secret-1"), "the first request had no cookie: {first:?}");
    for o in [&out, &out2] {
        let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(!text.contains("abc-secret-1") && !text.contains("ls-secret-2"), "a value is in the output");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_held_default_profile_runs_in_memory_and_a_held_named_one_fails() {
    let (base, seen) = serve();
    let dir = temp("held");
    let data = dir.join("data");
    let (held, _) = bare_drive::profile::Profile::pick(None, Some(&data)).unwrap();
    assert!(held.is_some());
    let out = drive(&dir, &data, &base, "open ${URL}/login\nexpect cookie sid\n", &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("profile default not used") && err.contains("in memory"), "{err}");
    // The run in memory saved nothing: a new run of the profile (free now) sends no cookie.
    drop(held);
    let out = drive(&dir, &data, &base, "open ${URL}/after\n", &[]);
    assert!(out.status.success());
    assert!(seen.lock().unwrap().iter().any(|(p, c)| p == "/after" && c == "none"));
    let (_work, _) = bare_drive::profile::Profile::pick(Some("work"), Some(&data)).unwrap();
    let out = drive(&dir, &data, &base, "open ${URL}/x\n", &["--profile", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("profile work: the profile is in use by process"));
    let _ = std::fs::remove_dir_all(dir);
}
