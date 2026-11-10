//! `save-state` and `open URL state="FILE"`: a login is saved (cookies, `localStorage`, `sessionStorage`), a new session starts from the
//! file and sends the cookie on its first request, the file is private, and no report holds a value. The server is a loopback server of
//! this test.

use bare_drive::run::{Options, Redactor, Report, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
            ("Set-Cookie: sid=abc-secret-1; Path=/; HttpOnly\r\n", "<p>in</p><script>localStorage.setItem('a','ls-secret-2');sessionStorage.setItem('t','ss-secret-3')</script>".to_string())
        } else {
            ("", format!("<p id=c>cookie: {cookie}</p>"))
        };
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

fn journey(base: &str, file: &str, text: &str) -> (Report, String) {
    let vars = HashMap::from([("URL".into(), base.to_string()), ("F".into(), file.to_string()), ("LS".into(), "ls-secret-2".into()), ("SS".into(), "ss-secret-3".into())]);
    let steps = parse(text, &vars).unwrap();
    let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
    let mut red = Redactor::default();
    let rep = run(&steps, &opt, &mut red);
    let json = rep.json(&red);
    (rep, json)
}

fn failure(rep: &Report) -> Option<String> {
    rep.failure.as_ref().map(|(l, f, _)| format!("L{l} {}: {}", f.kind, f.message))
}

fn file(name: &str) -> String {
    let p = std::env::temp_dir().join(format!("bare-state-{}-{name}.json", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned()
}

#[test]
fn a_saved_login_starts_a_new_session_and_the_file_is_private() {
    let (base, seen) = serve();
    let f = file("login");
    let (rep, json1) = journey(&base, &f, "open ${URL}/login\nexpect cookie sid\nexpect js \"localStorage.a==='${LS}'\"\nexpect js \"sessionStorage.t==='${SS}'\"\nsave-state \"${F}\"\n");
    assert!(rep.ok, "{:?}", failure(&rep));
    assert!(json1.contains("saved 1 cookies, 1 origins"), "{json1}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let (rep, json2) = journey(&base, &f, "open ${URL}/second state=\"${F}\"\nexpect cookie sid\nexpect js \"localStorage.a==='${LS}'\"\nexpect js \"sessionStorage.t==='${SS}'\"\nexpect css=\"#c\" text~\"sid=\"\n");
    assert!(rep.ok, "{:?}", failure(&rep));
    let first = seen.lock().unwrap().iter().find(|(p, _)| p == "/second").cloned().unwrap();
    assert!(first.1.contains("sid=abc-secret-1"), "the first request had no cookie: {first:?}");
    for v in ["abc-secret-1", "ls-secret-2", "ss-secret-3"] {
        assert!(!json1.contains(v) && !json2.contains(v), "a value is in a report: {v}");
    }
    let _ = std::fs::remove_file(&f);
}

#[test]
fn a_playwright_style_file_loads() {
    let (base, seen) = serve();
    let f = file("pw");
    let json = format!(
        r#"{{"cookies":[{{"name":"pw","value":"pw-secret-4","domain":"127.0.0.1","path":"/","expires":-1,"httpOnly":false,"secure":false,"sameSite":"Lax"}}],"origins":[{{"origin":"{base}","localStorage":[{{"name":"a","value":"ls-secret-2"}}]}}],"indexedDB":[]}}"#
    );
    std::fs::write(&f, json).unwrap();
    let (rep, out) = journey(&base, &f, "open ${URL}/pw state=\"${F}\"\nexpect cookie pw\nexpect js \"localStorage.a==='${LS}'\"\nexpect js \"sessionStorage.length===0\"\n");
    assert!(rep.ok, "{:?}", failure(&rep));
    assert!(seen.lock().unwrap().iter().any(|(p, c)| p == "/pw" && c.contains("pw=pw-secret-4")));
    assert!(!out.contains("pw-secret-4"));
    let _ = std::fs::remove_file(&f);
}

#[test]
fn a_missing_or_bad_file_fails_the_open_cleanly() {
    let (base, _) = serve();
    let f = file("missing");
    let (rep, _) = journey(&base, &f, "open ${URL}/x state=\"${F}\"\n");
    assert!(!rep.ok);
    let (_, fl, _) = rep.failure.as_ref().unwrap();
    assert!(fl.message.contains(&f), "{}", fl.message);
    std::fs::write(&f, "{\"cookies\":[{\"name\":\"a\",\"value\":\"v-secret\"}]}").unwrap();
    let (rep, out) = journey(&base, &f, "open ${URL}/x state=\"${F}\"\n");
    let msg = failure(&rep).unwrap();
    assert!(msg.contains("cookies[0].domain") && !msg.contains("v-secret") && !out.contains("v-secret"), "{msg}");
    let _ = std::fs::remove_file(&f);
}
