//! The `fill` step: one edit for the whole text (Chrome's `Input.insertText`, the call of Playwright `fill`), not one key for each character.
//! The page counts the key and input events of its field. `#now` is swapped on focus, as the Vue search of Wikipedia does
//! (`swapped_input.rs`): the text goes to the new focused field. The server is on loopback.

use std::io::{Read, Write};
use std::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>fill</title></head><body>
<input id="a" value="old"><textarea id="t"></textarea><div><input id="now"></div>
<script>
window.keys = 0; window.inputs = 0; window.last = '';
['keydown', 'keypress', 'keyup'].forEach(function (t) { document.addEventListener(t, function (e) { if (e.key !== 'Control' && e.key !== 'a') window.keys++ }, true) });
document.addEventListener('input', function (e) { window.inputs++; window.last = e.inputType + ':' + e.data }, true);
var now = document.getElementById('now');
now.addEventListener('focus', function () {
  var box = now.parentNode, input = document.createElement('input');
  box.innerHTML = ''; input.id = 'now'; box.appendChild(input); input.focus();
}, { once: true });
</script></body></html>"#;

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
    let dir = std::env::temp_dir().join(format!("bare-drive-fill-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, steps.replace("ORIGIN", &serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x200"]).arg(&file).output().unwrap();
    (run.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)))
}

#[test]
fn fill_replaces_the_text_with_one_input_event_and_no_typed_keys() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#a\" \"hello world\"\nexpect css=\"#a\" value=\"hello world\"\n\
        expect js \"window.keys === 0 && window.inputs === 1 && window.last === 'insertText:hello world'\"\n";
    let (code, out) = drive("one", steps);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn fill_with_no_text_clears_the_field() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#a\" \"\"\nexpect css=\"#a\" value=\"\"\n";
    let (code, out) = drive("clear", steps);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn fill_keeps_line_feeds_in_a_textarea() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#t\" \"a\\nb\"\nexpect js \"document.getElementById('t').value === 'a\\\\nb' && window.inputs === 1\"\n";
    let (code, out) = drive("lines", steps);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn fill_goes_to_the_field_that_the_page_put_in_place_on_focus() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#now\" \"rust\"\nexpect css=\"#now\" value=\"rust\"\n";
    let (code, out) = drive("swap", steps);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn fill_with_keys_last_inserts_all_but_the_last_character_then_types_it() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#a\" \"hello\" keys=last\nexpect css=\"#a\" value=\"hello\"\n\
        expect js \"window.keys === 3 && window.inputs === 2 && window.last === 'insertText:o'\"\n";
    let (code, out) = drive("last", steps);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn fill_with_keys_last_types_a_text_of_one_character() {
    let steps = "timeout 30000\nopen ORIGIN/\nfill css=\"#a\" \"x\" keys=last\nexpect css=\"#a\" value=\"x\"\n\
        expect js \"window.keys === 3 && window.inputs === 1\"\n";
    let (code, out) = drive("last1", steps);
    assert_eq!(code, 0, "{out}");
}
