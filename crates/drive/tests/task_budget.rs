//! A live page gives one task far more than the 100M steps of `EvalLimits::default()`. A large app (Gmail) runs its first render
//! in one task. With the small budget bare aborted that task and the app never showed. Issue #454.

use bare::page::Page;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

/// One inline script that runs about 150M steps (3 steps for each turn of the loop), then marks the page.
const INDEX: &str = r#"<!doctype html><html><body><p id="out">none</p><script>
var n = 0; for (var i = 0; i < 5e7; i++) { n += i }
document.getElementById('out').textContent = 'done'
</script></body></html>"#;

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{INDEX}", INDEX.len());
            let _ = s.write_all(r.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/")
}

#[test]
fn a_live_page_task_may_run_more_than_100m_steps() {
    let origin = serve();
    let out = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&origin, (800, 600)).unwrap();
            p.wait_load(Duration::from_secs(120)).unwrap();
            let b = p.browser().unwrap();
            let v = b.run_script("document.getElementById('out').textContent").unwrap();
            b.it.display(v)
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(out, "done", "the long task was aborted by the step budget");
}
