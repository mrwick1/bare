//! A search box that a script swaps on focus, as the Vue search of Wikipedia does: the focus handler builds a new input with an SVG icon
//! (`g.innerHTML`), puts it in the place of the old one, copies the value, and moves the focus to it. `#now` swaps in the focus event;
//! `#later` swaps in a timer after the first key, as Wikipedia does when its widget has loaded, so the swap comes while the text is typed. In a browser the keys
//! go to the focused field: typed text ends in the new input, and Enter submits it. `#ready` swaps on focus and holds a value, so a `press` with
//! its locator focuses it and the page swaps it. `#bare` swaps after the first key into an input with no id, as the second swap of
//! Wikipedia does, so the locator of the old field matches nothing: a `type` or `press` with it goes to the focused field. The server is on loopback.

use std::io::{Read, Write};
use std::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>home</title></head><body>
<form action="/search"><div><input id="now" name="q"></div></form>
<form action="/search"><div><input id="later" name="q"></div></form>
<form action="/search"><div><input id="ready" name="q" value="rust"></div></form>
<form action="/search"><div><input id="bare" name="q"></div></form>
<script>
function swap(old, id) {
  var box = old.parentNode, value = old.value;
  box.innerHTML = '';
  var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'), g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.innerHTML = '<path d="M0 0h1v1z"/>';
  svg.appendChild(g);
  var input = document.createElement('input');
  if (id) input.id = old.id;
  input.name = 'q'; input.value = value;
  box.appendChild(svg); box.appendChild(input);
  input.focus();
}
var now = document.getElementById('now'), later = document.getElementById('later'), ready = document.getElementById('ready'), bare = document.getElementById('bare');
now.addEventListener('focus', function () { swap(now, true) }, { once: true });
later.addEventListener('input', function () { setTimeout(function () { swap(later, true) }, 0) }, { once: true });
ready.addEventListener('focus', function () { swap(ready, true) }, { once: true });
bare.addEventListener('input', function () { setTimeout(function () { swap(bare, false) }, 0) }, { once: true });
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
                let body = match path.strip_prefix("/search?q=") {
                    Some(q) => format!("<!doctype html><title>result {q}</title>"),
                    None => PAGE.to_string(),
                };
                let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn drive(name: &str, steps: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-swapped-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, steps.replace("ORIGIN", &serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x120"]).arg(&file).output().unwrap();
    (run.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr)))
}

#[test]
fn typing_into_an_input_that_is_swapped_on_focus_keeps_the_text_and_enter_submits() {
    for id in ["now", "later"] {
        let steps = format!(
            "timeout 30000\nopen ORIGIN/\ntype css=\"#{id}\" \"rust\"\nexpect css=\"#{id}\" value=\"rust\"\nexpect css=\"svg path\" count 1\npress Enter\nexpect title=\"result rust\"\n"
        );
        let (code, out) = drive(id, &steps);
        assert_eq!(code, 0, "{id}: {out}");
    }
}

#[test]
fn press_with_the_locator_of_a_replaced_field_goes_to_the_new_focused_field() {
    // Swapped by the focus of the press: the key goes to the new field.
    let (code, out) = drive("ready", "timeout 30000\nopen ORIGIN/\npress css=\"#ready\" Enter\nexpect title=\"result rust\"\n");
    assert_eq!(code, 0, "ready: {out}");
    // Swapped while typed, into a field that the locator no longer matches: the key goes to the focused field.
    let (code, out) = drive("bare", "timeout 30000\nopen ORIGIN/\ntype css=\"#bare\" \"rust\"\nexpect css=\"svg path\" count 1\npress css=\"#bare\" Enter\nexpect title=\"result rust\"\n");
    assert_eq!(code, 0, "bare: {out}");
}

#[test]
fn type_with_the_locator_of_a_replaced_field_goes_to_the_new_focused_field() {
    // `#bare` is swapped after the first key into an input with no id: the second `type` with the old locator types into the focused field.
    let (code, out) = drive("bare-type", "timeout 30000\nopen ORIGIN/\ntype css=\"#bare\" \"rust\"\nexpect css=\"svg path\" count 1\ntype css=\"#bare\" \"go\"\npress Enter\nexpect title=\"result go\"\n");
    assert_eq!(code, 0, "bare-type: {out}");
}
