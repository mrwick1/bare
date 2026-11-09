//! A runaway script (the step budget ends it) does not stop the page: a later click, type and timer still work, as in Chrome.
//! A page that aborts task after task is stopped, and the failing step names the reason. The server is on loopback.

use std::io::{Read, Write};
use std::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>first</title></head><body><button id="go">go</button><input id="q"><p id="out">none</p><script>
document.getElementById('go').addEventListener('click', function () { document.getElementById('out').textContent = 'clicked' });
document.getElementById('q').addEventListener('input', function (e) { document.title = 'typed ' + e.target.value });
setTimeout(function () { document.body.setAttribute('data-timer', 'ran') }, 20);
</script><script>
document.body.appendChild(document.createElement('hr')); while (true) {}
</script></body></html>"#;

const CHAIN: &str = r#"<!doctype html><html><body><input id="q"><script>
(function again() { setTimeout(again, 0); while (true) {} })();
</script></body></html>"#;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let path = String::from_utf8_lossy(&buf[..n]).split(' ').nth(1).unwrap_or("/").to_string();
                let body = if path == "/chain" { CHAIN } else { PAGE };
                let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn drive(name: &str, steps: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-survive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, steps.replace("ORIGIN", &serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x120"]).arg(&file).output().unwrap();
    (run.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)))
}

#[test]
fn input_and_timers_work_after_a_runaway_script() {
    let (code, out) = drive("one", "timeout 60000\nopen ORIGIN/\nexpect css=\"hr\" count 1\nexpect css=\"body[data-timer=ran]\" count 1\nclick css=\"#go\"\nexpect css=\"#out\" text=\"clicked\"\ntype css=\"#q\" \"ab\"\nexpect title=\"typed ab\"\n");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Steps"), "the console error is in the report: {out}");
}

#[test]
fn a_page_that_aborts_task_after_task_fails_the_step_with_the_reason() {
    // Six runaway tasks of 100 M steps each: about 10 s.
    let (code, out) = drive("chain", "timeout 120000\nopen ORIGIN/chain\nexpect css=\"#never\" count 1\n");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("the page stopped") && out.contains("aborted by script budgets"), "{out}");
}
