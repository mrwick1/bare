//! An idle window sleeps. While a reply is in flight, `Page::tick` does not ask to be called again in a few ms: the worker that
//! gets the reply wakes the loop. The tests run the loop of the window on a channel (the wake pipe) and count the ticks and the CPU.
//! A loopback server holds `/hang` for the time in `HANG`.

use bare::page::Page;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const INDEX: &str = r#"<!doctype html><html><body><p id="out">none</p><script>
fetch('/hang').then(function (r) { return r.text() }).then(function (t) { document.getElementById('out').textContent = t })
</script></body></html>"#;

fn serve(hang: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let path = String::from_utf8_lossy(&buf[..n]).split(' ').nth(1).unwrap_or("/").to_string();
                let body = if path == "/hang" {
                    std::thread::sleep(hang);
                    "late"
                } else {
                    INDEX
                };
                let r = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The wake hook is one for the process, and a tick count must not see the wakes of another test: the tests run one at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// CPU time of the whole process (user + system) in seconds, from `/proc/self/stat` (100 ticks a second on Linux).
fn cpu() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let f: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
    (f[11].parse::<f64>().unwrap() + f[12].parse::<f64>().unwrap()) / 100.0
}

/// The loop of the window: tick, then sleep until the time that the tick gave or until a wake. Gives the ticks made in `span`, and
/// whether the page text was filled by the end.
fn run_loop(p: &mut Page, rx: &mpsc::Receiver<()>, span: Duration) -> (u32, bool) {
    let end = Instant::now() + span;
    let mut ticks = 0;
    while Instant::now() < end {
        let (_, next) = p.tick();
        ticks += 1;
        let wait = next.map_or(end, |n| n.min(end)).saturating_duration_since(Instant::now());
        let _ = rx.recv_timeout(wait);
    }
    let b = p.browser().unwrap();
    let v = b.run_script("document.getElementById('out').textContent").unwrap();
    (ticks, b.it.display(v) == "late")
}

fn open(origin: &str) -> (Page, mpsc::Receiver<()>) {
    let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
    p.wait_load(std::time::Duration::from_secs(10)).unwrap();
    p.quiet = true;
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    p.wake_on_done(move || { let _ = tx.lock().unwrap().send(()); });
    (p, rx)
}

#[test]
fn a_request_in_flight_does_not_wake_the_loop_every_few_ms() {
    let origin = serve(Duration::from_millis(1500));
    big(move || {
        let (mut p, rx) = open(&origin);
        let (ticks, _) = run_loop(&mut p, &rx, Duration::from_secs(1));
        assert!(ticks < 5, "{ticks} ticks in 1 s with a request in flight (about 250 when it polls every 4 ms)");
    });
}

#[test]
fn the_reply_wakes_the_loop_at_once() {
    let origin = serve(Duration::from_millis(600));
    big(move || {
        let (mut p, rx) = open(&origin);
        let t = Instant::now();
        let (_, filled) = run_loop(&mut p, &rx, Duration::from_millis(900));
        assert!(filled, "the reply did not reach the page in 900 ms (it comes at 600 ms)");
        assert!(t.elapsed() < Duration::from_millis(1000));
    });
}

#[test]
fn a_window_with_a_10_s_request_in_flight_uses_almost_no_cpu() {
    let origin = serve(Duration::from_secs(10));
    big(move || {
        let (mut p, rx) = open(&origin);
        let (c0, t0) = (cpu(), Instant::now());
        let (ticks, _) = run_loop(&mut p, &rx, Duration::from_secs(10));
        let (used, wall) = (cpu() - c0, t0.elapsed().as_secs_f64());
        eprintln!("idle with a pending fetch: {ticks} ticks, {used:.3} s CPU in {wall:.1} s ({:.2}%)", used / wall * 100.0);
        assert!(used / wall < 0.005, "{used:.3} s CPU in {wall:.1} s");
    });
}
