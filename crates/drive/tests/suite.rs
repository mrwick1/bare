//! `bare-drive --suite DIR --workers K`: the login runs once and the shared tests start from it, an own test logs in itself, a test never
//! sees the cookies or the storage of another, a failed test ends its worker and the next test gets a new process, and a test that
//! shares a login it should not share fails (no false pass). The server is a loopback server of this test.

use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Site {
    sessions: HashSet<String>,
    login_posts: usize,
    next: usize,
}

fn serve() -> (String, Arc<Mutex<Site>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let site: Arc<Mutex<Site>> = Default::default();
    let shared = site.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let site = shared.clone();
            std::thread::spawn(move || connection(s, site));
        }
    });
    (base, site)
}

fn connection(mut s: TcpStream, site: Arc<Mutex<Site>>) {
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
        let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")?.trim().parse().ok()).unwrap_or(0);
        while buf.len() < end + len {
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&piece[..n]),
            }
        }
        buf.drain(..end + len);
        let (method, path) = (head.split(' ').next().unwrap_or(""), head.split(' ').nth(1).unwrap_or("/"));
        let cookie = head.lines().find_map(|l| l.strip_prefix("Cookie: ")).unwrap_or("");
        let sid = cookie.split(';').find_map(|c| c.trim().strip_prefix("sid=")).unwrap_or("").to_string();
        let mut site = site.lock().unwrap();
        let valid = site.sessions.contains(&sid);
        let (code, extra, body) = match (method, path) {
            ("POST", "/login") => {
                site.login_posts += 1;
                site.next += 1;
                let sid = format!("s{}", site.next);
                site.sessions.insert(sid.clone());
                ("303 See Other", format!("Set-Cookie: sid={sid}; Path=/; HttpOnly\r\nLocation: /home\r\n"), String::new())
            }
            ("POST", "/logout") => {
                site.sessions.remove(&sid);
                ("303 See Other", "Set-Cookie: sid=; Path=/; Max-Age=0\r\nLocation: /login\r\n".into(), String::new())
            }
            (_, "/login") => ("200 OK", String::new(), "<title>login</title><h1>Sign in</h1><form method=post action=/login><input name=email><input name=password type=password><button>Sign in</button></form>".into()),
            (_, "/home") if valid => ("200 OK", String::new(), "<title>home</title><h1>Heading home</h1><a href=/p1>Page 1</a><form method=post action=/logout><button>Sign out</button></form>".into()),
            (_, "/p1") if valid => ("200 OK", String::new(), "<title>p1</title><h1>Heading p1</h1>".into()),
            (_, "/home" | "/p1") => ("303 See Other", "Location: /login\r\n".into(), String::new()),
            (_, "/__stats") => ("200 OK", String::new(), site.login_posts.to_string()),
            _ => ("404 Not Found", String::new(), "no".into()),
        };
        drop(site);
        let r = format!("HTTP/1.1 {code}\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

const SETUP: &str = "open ${BASE}/login\ntype css=\"input[name=email]\" \"a@b.test\"\ntype css=\"input[name=password]\" \"pw\"\nclick role=button name=\"Sign in\"\nexpect css=\"h1\" text=\"Heading home\"\nsave-state \"${STATE}\"\n";
const START: &str = "open ${BASE}/home state=\"${STATE}\"\nexpect css=\"h1\" text=\"Heading home\"\n";

fn suite(tag: &str, tests: &[(&str, String)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bare-suite-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("setup.steps"), SETUP).unwrap();
    for (name, text) in tests {
        std::fs::write(dir.join(format!("{name}.steps")), text).unwrap();
    }
    dir
}

fn run(dir: &Path, base: &str, workers: &str) -> (Value, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_bare-drive"))
        .args(["--quiet", "--suite"])
        .arg(dir)
        .args(["--workers", workers, "--timeout", "4000", "--test-timeout", "40000", "--var"])
        .arg(format!("BASE={base}"))
        .arg("--out")
        .arg(dir.join("out"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    (serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text} {}", String::from_utf8_lossy(&out.stderr))), out.status.code().unwrap_or(-1))
}

fn row<'a>(j: &'a Value, name: &str) -> &'a Value {
    j["results"].as_array().unwrap().iter().find(|r| r["test"] == name).unwrap_or_else(|| panic!("no row {name}: {j}"))
}

#[test]
fn shared_tests_start_from_one_login_and_an_own_test_logs_in_itself() {
    let (base, site) = serve();
    let page = format!("{START}click role=link name=\"Page 1\"\nexpect css=\"h1\" text=\"Heading p1\"\n");
    let dir = suite(
        "shared",
        &[
            ("a-read", page.clone()),
            ("b-read", page),
            ("c-signout", format!("{START}click role=button name=\"Sign out\"\nexpect url~\"/login\"\nexpect cookie sid absent\n")),
            ("d-read", START.to_string()),
        ],
    );
    let (j, code) = run(&dir, &base, "1");
    assert_eq!(code, 0, "{j}");
    assert_eq!(j["passed"], 4);
    assert_eq!(row(&j, "a-read")["class"], "shared");
    assert_eq!(row(&j, "c-signout")["class"], "own");
    assert_eq!(row(&j, "c-signout")["why"], "a step says sign out");
    // One login for the setup and one for the own test: the other three tests did not log in.
    assert_eq!(site.lock().unwrap().login_posts, 2);
    assert_eq!(j["logins_expected"], 2);
    // The saved session (live credentials) is not left behind.
    assert!(!dir.join("out/state.json").exists());
}

#[test]
fn a_test_does_not_see_the_cookies_or_storage_of_the_test_before_it() {
    let (base, _) = serve();
    let a = format!("{START}expect js \"(localStorage.setItem('leak','1'), sessionStorage.setItem('leak','1'), true)\"\ncookie set leak 1\nexpect cookie leak value=\"1\"\n");
    let b = format!("{START}expect js \"localStorage.getItem('leak') === null && sessionStorage.getItem('leak') === null\"\nexpect cookie leak absent\nexpect cookie sid\n");
    let dir = suite("iso", &[("a-leaves", a), ("b-sees-nothing", b)]);
    // One worker: both tests run in the same process, one after the other.
    let (j, code) = run(&dir, &base, "1");
    assert_eq!((code, row(&j, "b-sees-nothing")["ok"].clone(), row(&j, "b-sees-nothing")["worker"].clone()), (0, true.into(), 0.into()), "{j}");
    assert_eq!(row(&j, "a-leaves")["worker"], 0);
}

#[test]
fn a_failed_test_ends_its_worker_and_the_next_test_gets_a_new_process() {
    let (base, _) = serve();
    let bad = format!("timeout 600\n{START}expect css=\"h1\" text=\"not there\"\n");
    let dir = suite("fail", &[("a-ok", START.to_string()), ("b-fails", bad), ("c-after", START.to_string())]);
    let (j, code) = run(&dir, &base, "1");
    assert_eq!(code, 1, "{j}");
    assert_eq!((j["passed"].clone(), j["failed"].clone()), (2.into(), 1.into()));
    assert_eq!(row(&j, "b-fails")["ok"], false);
    assert_eq!(row(&j, "b-fails")["failure"]["kind"], "expect");
    // The setup and the first two tests ran in worker 0, which ended with the failure; the next test ran in worker 1.
    assert_eq!((row(&j, "a-ok")["worker"].clone(), row(&j, "b-fails")["worker"].clone(), row(&j, "c-after")["worker"].clone()), (0.into(), 0.into(), 1.into()));
    assert_eq!(j["worker_stats"].as_array().unwrap().len(), 2);
}

#[test]
fn a_test_that_shares_a_login_it_should_not_share_makes_the_next_test_fail() {
    // `# shared` forces a sign-out test onto the shared session: the server ends it, and the test after it must not pass on a dead session.
    let (base, _) = serve();
    let forced = format!("# shared\n{START}click role=button name=\"Sign out\"\nexpect url~\"/login\"\n");
    let dir = suite("stale", &[("a-forced", forced), ("b-after", format!("timeout 600\n{START}"))]);
    let (j, code) = run(&dir, &base, "1");
    assert_eq!(code, 1, "{j}");
    assert_eq!(row(&j, "a-forced")["class"], "shared");
    assert_eq!(row(&j, "b-after")["ok"], false, "{j}");
    assert_eq!(j["ok"], false);
}

#[test]
fn workers_run_the_queue_in_parallel() {
    let (base, _) = serve();
    let tests: Vec<(String, String)> = (0..6).map(|i| (format!("t{i}"), START.to_string())).collect();
    let tests: Vec<(&str, String)> = tests.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
    let dir = suite("par", &tests);
    let (j, code) = run(&dir, &base, "3");
    assert_eq!((code, j["passed"].clone(), j["workers"].clone()), (0, 6.into(), 3.into()), "{j}");
    let used: HashSet<u64> = j["results"].as_array().unwrap().iter().filter_map(|r| r["worker"].as_u64()).collect();
    assert!(used.len() > 1, "{j}");
}
