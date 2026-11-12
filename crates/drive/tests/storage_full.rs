//! A page that fills `localStorage` past the bounds of bare keeps running, as in a browser: each write that does not fit throws a
//! `QuotaExceededError`, and the page catches it or loses only that script. The first write is a module cache of 1.2M units, as Wikipedia
//! stores (issue #405); the loop then writes more keys than the store takes. The server is on loopback.

use std::io::{Read, Write};
use std::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>first</title></head><body><button id="go">go</button><p id="out">none</p><script>
localStorage.setItem('modules', new Array(1200001).join('x'));
var failed = 0;
for (var i = 0; i < 1100; i++) { try { localStorage.setItem('k' + i, 'v') } catch (e) { failed++ } }
document.title = 'stored ' + localStorage.length + ' failed ' + failed;
document.getElementById('go').addEventListener('click', function () { document.getElementById('out').textContent = 'clicked' });
setTimeout(function () { document.body.setAttribute('data-timer', 'ran') }, 20);
</script><script>
localStorage.setItem('one-more', 'v');
document.title = 'not reached';
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

#[test]
fn a_page_that_fills_local_storage_keeps_running() {
    let dir = std::env::temp_dir().join(format!("bare-drive-storage-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    // The store takes 1024 keys: the module cache and 1023 of the loop.
    let steps = "timeout 30000\nopen ORIGIN/\nexpect title=\"stored 1024 failed 77\"\nexpect css=\"body[data-timer=ran]\" count 1\nclick css=\"#go\"\nexpect css=\"#out\" text=\"clicked\"\n";
    std::fs::write(&file, steps.replace("ORIGIN", &serve())).unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x120"]).arg(&file).output().unwrap();
    let out = format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.status.code(), Some(0), "{out}");
}
