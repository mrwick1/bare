//! `nth=N` picks one of several matches (from 0, in tree order). With no `nth`, one rendered match among hidden twins is the target
//! (Wikipedia has a visible and a hidden `input[name=search]`); two rendered matches stay `duplicate`. The server is on loopback.

use std::io::{Read, Write};
use std::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>nth</title></head><body>
<button class="b" onclick="window.hit='a'">A</button><button class="b" onclick="window.hit='b'">B</button>
<input id="s1" name="q"><input id="s2" name="q" style="display:none">
<input name="v"><input name="v">
</body></html>"#;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{PAGE}", PAGE.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn drive(name: &str, steps: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-nth-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, format!("timeout 1500\nopen {}/\n{steps}", serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x200"]).arg(&file).output().unwrap();
    (run.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)))
}

#[test]
fn nth_0_picks_the_first_match() {
    let (code, out) = drive("first", "click css=\"button.b\" nth=0\nexpect js \"window.hit === 'a'\"\n");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn nth_1_picks_the_second_match() {
    let (code, out) = drive("second", "click css=\"button.b\" nth=1\nexpect js \"window.hit === 'b'\"\n");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn nth_past_the_last_match_says_how_many_match() {
    let (code, out) = drive("range", "click css=\"button.b\" nth=2\n");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("\"kind\":\"not_found\"") && out.contains("only 2 element(s) match css=\\\"button.b\\\""), "{out}");
}

#[test]
fn one_rendered_match_among_hidden_twins_is_the_target() {
    let (code, out) = drive("visible", "fill css=\"input[name=q]\" \"x\"\nexpect css=\"#s1\" value=\"x\"\n");
    assert_eq!(code, 0, "{out}");
}

#[test]
fn two_rendered_matches_with_no_nth_stay_duplicate() {
    let (code, out) = drive("dup", "fill css=\"input[name=v]\" \"x\"\n");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("\"kind\":\"duplicate\""), "{out}");
}
