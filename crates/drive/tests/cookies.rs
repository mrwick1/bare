//! Cookies in a journey: the server sets cookies, the next document sends them back, `cookie set` and `expect cookie` work, and the JSON
//! dump of the jar shows the names and the flags and never a value. The server is a loopback server of this test.

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || connection(s));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream) {
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
        let head = String::from_utf8_lossy(&seen[..end]).to_string();
        seen.drain(..end);
        let path = head.split(' ').nth(1).unwrap_or("/").to_string();
        let cookie = head.lines().find_map(|l| l.strip_prefix("Cookie: ")).unwrap_or("none").to_string();
        let r = if path == "/login" {
            let body = "<p>logged in</p>";
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: sid=abc-secret-1; Path=/; HttpOnly\r\nSet-Cookie: vis=2; Path=/\r\nSet-Cookie: api_token=tok-value-9; Path=/; Secure; SameSite=None\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        } else {
            let body = format!("<p id=c>cookie: {cookie}</p>");
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        };
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

fn journey(text: &str) -> (bare_drive::run::Report, String) {
    let base = serve();
    let vars = HashMap::from([("URL".to_string(), base), ("V".to_string(), "a=b".to_string())]);
    let steps = parse(text, &vars).unwrap();
    let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
    let mut red = Redactor::default();
    red.add("tok-value-9");
    let rep = run(&steps, &opt, &mut red);
    let json = rep.json(&red);
    (rep, json)
}

#[test]
fn a_login_cookie_comes_back_and_the_steps_see_and_set_cookies() {
    let (rep, json) = journey(
        r##"
open ${URL}/login
expect cookie sid
expect cookie vis value="2"
expect cookie vis value~"2"
expect cookie nosuch absent
goto ${URL}/page
expect css="#c" text~"cookie: sid="
expect js "document.cookie.includes('vis=2') && !document.cookie.includes('sid')"
cookie set mine "${V}" path=/
expect cookie mine value="${V}"
goto ${URL}/page
expect css="#c" text~"; mine="
"##,
    );
    let failure = rep.failure.as_ref().map(|(l, f, _)| format!("L{l} {}: {}", f.kind, f.message));
    assert!(rep.ok, "{failure:?}");
    // The dump has names and flags, and no value at all.
    assert!(json.contains("\"cookies\":["), "{json}");
    assert!(json.contains("\"name\":\"sid\"") && json.contains("\"http_only\":true") && json.contains("\"name\":\"mine\""), "{json}");
    for v in ["abc-secret-1", "tok-value-9", "a=b"] {
        assert!(!json.contains(v), "a value is in the dump: {v}");
    }
}

#[test]
fn a_failed_cookie_check_names_the_cookie_and_never_its_value() {
    let (rep, json) = journey("open ${URL}/login\ntimeout 300\nexpect cookie sid value=\"nope\"\n");
    assert!(!rep.ok);
    let (_, f, _) = rep.failure.as_ref().unwrap();
    assert_eq!(f.kind, "expect");
    assert!(f.message.contains("sid") && !f.message.contains("abc-secret-1"), "{}", f.message);
    assert!(!json.contains("abc-secret-1"));
    let (rep, _) = journey("open ${URL}/login\ntimeout 300\nexpect cookie vis absent\n");
    assert!(!rep.ok);
}

#[test]
fn each_open_is_a_new_profile_with_an_empty_jar() {
    let (rep, _) = journey("open ${URL}/login\nexpect cookie sid\nopen ${URL}/page\nexpect css=\"#c\" text~\"cookie: none\"\nexpect cookie sid absent\n");
    let failure = rep.failure.as_ref().map(|(l, f, _)| format!("L{l} {}: {}", f.kind, f.message));
    assert!(rep.ok, "{failure:?}");
}
