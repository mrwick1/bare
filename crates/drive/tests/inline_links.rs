//! A plain inline `<a>` has no box fragment, only text fragments. `visible`, `click` and `type` must still work on it, also when it
//! wraps over two lines (`tools/fixtures/drive/inline.html`). A `display: none` link stays hidden.

use std::io::{Read, Write};
use std::net::TcpListener;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let page = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/drive/inline.html")).unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let page = page.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", page.len());
                let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&page));
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn drive(name: &str, steps: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-inline-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, format!("open {}/\n{steps}\n", serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x300", "--out", dir.join("out").to_str().unwrap()]).arg(&file).output().unwrap();
    let json = String::from_utf8_lossy(&run.stdout).to_string();
    let _ = std::fs::remove_dir_all(dir);
    (run.status.code().unwrap_or(-1), json)
}

#[test]
fn an_inline_link_is_visible_and_clickable() {
    let (code, json) = drive("one", "expect role=link name=\"the link\" visible\nclick role=link name=\"the link\"\nexpect url~\"#one-hit\"");
    assert_eq!(code, 0, "{json}");
}

#[test]
fn a_link_that_wraps_over_lines_is_visible_and_clickable() {
    let (code, json) = drive("wrap", "expect css=\"#wrap\" visible\nexpect js \"document.getElementById('wrap').getClientRects().length > 1\"\nclick css=\"#wrap\"\nexpect url~\"#wrap-hit\"");
    assert_eq!(code, 0, "{json}");
}

#[test]
fn a_link_with_display_none_stays_hidden() {
    let (code, json) = drive("gone", "expect css=\"#gone\" hidden");
    assert_eq!(code, 0, "{json}");
}
