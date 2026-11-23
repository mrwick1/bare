//! An open WebSocket and the loop of a window. A message that comes on the socket wakes the loop at once, as a reply does
//! (`Page::wake_on_done`). An idle open socket does not make the loop tick.
//! A loopback server serves the page, then takes the upgrade of `/ws` and sends `late` after `DELAY`.

use bare::page::Page;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const INDEX: &str = r#"<!doctype html><html><body><p id="out">none</p><script>
var ws = new WebSocket('/ws');
ws.onopen = function () { document.getElementById('out').textContent = 'open' };
ws.onmessage = function (e) { document.getElementById('out').textContent = e.data };
</script></body></html>"#;

fn b64(input: &[u8]) -> String {
    const D: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((*c.get(1).unwrap_or(&0) as u32) << 8) | *c.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() { D[((n >> shift) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Serve the page, and on `/ws` the upgrade, then the text `late` after `delay` (none when `delay` is `None`). The socket stays open.
fn serve(delay: Option<Duration>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read_exact(&mut b).is_err() {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                if let Some(key) = head.lines().find_map(|l| l.strip_prefix("Sec-WebSocket-Key: ")) {
                    let accept = b64(&bare_tls::sha1::sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes()));
                    let _ = write!(s, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
                    if let Some(d) = delay {
                        std::thread::sleep(d);
                        let _ = s.write_all(&[0x81, 4, b'l', b'a', b't', b'e']);
                    }
                    // Hold the socket open until the client goes.
                    let _ = s.read(&mut [0u8; 64]);
                    return;
                }
                let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{INDEX}", INDEX.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// The wake hook is one for the process: the tests run one at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn open(url: &str) -> (Page, mpsc::Receiver<()>) {
    let mut p = Page::open(url, (800, 600)).unwrap();
    p.wait_load(Duration::from_secs(10)).unwrap();
    p.quiet = true;
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    p.wake_on_done(move || { let _ = tx.lock().unwrap().send(()); });
    (p, rx)
}

fn out(p: &mut Page) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script("document.getElementById('out').textContent").unwrap();
    b.it.display(v)
}

/// Tick, then sleep until the time that the tick gave or a wake, until `out` is `want` or `span` ends. Gives the ticks and the time.
fn run_until(p: &mut Page, rx: &mpsc::Receiver<()>, want: &str, span: Duration) -> (u32, Option<Duration>) {
    let (t, end) = (Instant::now(), Instant::now() + span);
    let mut ticks = 0;
    while Instant::now() < end {
        let (_, next) = p.tick();
        ticks += 1;
        if out(p) == want {
            return (ticks, Some(t.elapsed()));
        }
        let wait = next.map_or(end, |n| n.min(end)).saturating_duration_since(Instant::now());
        let _ = rx.recv_timeout(wait);
    }
    (ticks, None)
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn a_socket_message_wakes_the_loop_at_once() {
    let url = serve(Some(Duration::from_millis(600)));
    big(move || {
        let (mut p, rx) = open(&url);
        assert!(run_until(&mut p, &rx, "open", Duration::from_secs(2)).1.is_some(), "the socket did not open");
        let (_, at) = run_until(&mut p, &rx, "late", Duration::from_secs(2));
        let at = at.expect("the message did not come");
        assert!(at < Duration::from_millis(800), "the message reached the page after {at:?} (it comes at about 600 ms)");
    });
}

#[test]
fn an_idle_open_socket_does_not_tick_the_loop() {
    let url = serve(None);
    big(move || {
        let (mut p, rx) = open(&url);
        assert!(run_until(&mut p, &rx, "open", Duration::from_secs(2)).1.is_some(), "the socket did not open");
        let (ticks, _) = run_until(&mut p, &rx, "never", Duration::from_secs(1));
        assert!(ticks < 5, "{ticks} ticks in 1 s with an idle socket open");
    });
}
