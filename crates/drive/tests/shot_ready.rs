//! `shot` waits until the page is ready (`Page::idle` on two looks in a row), so the picture shows what the scripts add. The page loads
//! an external script that fills `#a` and then fetches `/data` to fill `#b`; both replies are slow. A filled box is a black bar.

mod common;

use std::time::{Duration, Instant};

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/s.js" => {
            std::thread::sleep(Duration::from_millis(300));
            let js = "var a = document.getElementById('a'); a.style.background = '#000'; a.style.height = '40px'; \
                      fetch('/data').then(function (r) { return r.text() }).then(function (t) { var b = document.getElementById('b'); b.textContent = t; b.style.background = '#000'; b.style.height = '40px' })";
            (200, "application/javascript", js.as_bytes().to_vec())
        }
        "/data" => {
            std::thread::sleep(Duration::from_millis(300));
            (200, "text/plain", b"from fetch".to_vec())
        }
        "/hang" => {
            std::thread::sleep(Duration::from_secs(30));
            (200, "text/plain", b"late".to_vec())
        }
        "/stuck" => (200, "text/html", b"<!DOCTYPE html><title>t</title><p>text</p><script>fetch('/hang')</script>".to_vec()),
        _ => (200, "text/html", b"<!DOCTYPE html><title>t</title><body style=\"margin:0\"><div id=a></div><div id=b></div><script src=/s.js></script></body>".to_vec()),
    }
}

/// Run the steps against the server. Gives the pixels of `x.png` (width, `0xRRGGBB`), the JSON and the time it took.
fn run(path: &str, steps: &str) -> (usize, Vec<u32>, String, Duration) {
    let dir = std::env::temp_dir().join(format!("bare-drive-shot-ready-{}-{}", std::process::id(), path.len()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("steps");
    std::fs::write(&file, format!("open {}{path}\n{steps}\nshot x\n", common::serve_route(route))).unwrap();
    let out = dir.join("out");
    let start = Instant::now();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x120", "--out", out.to_str().unwrap()]).arg(&file).output().unwrap();
    let took = start.elapsed();
    let json = String::from_utf8_lossy(&run.stdout).to_string();
    assert_eq!(run.status.code(), Some(0), "{json} {}", String::from_utf8_lossy(&run.stderr));
    let mut r = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(out.join("x.png")).unwrap())).read_info().unwrap();
    let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
    let f = r.next_frame(&mut buf).unwrap();
    let px = buf[..f.buffer_size()].chunks_exact(3).map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])).collect();
    std::fs::remove_dir_all(dir).unwrap();
    (f.width as usize, px, json, took)
}

#[test]
fn shot_right_after_open_shows_what_the_scripts_add() {
    let (w, px, json, _) = run("/", "");
    assert_eq!(px[20 * w + 5], 0, "#a is not filled: {json}");
    assert_eq!(px[60 * w + 5], 0, "#b is not filled: {json}");
    assert!(!json.contains("not ready"), "{json}");
}

#[test]
fn shot_of_a_page_that_never_gets_idle_is_taken_at_the_limit_with_a_note() {
    let (_, px, json, took) = run("/stuck", "timeout 1500");
    assert!(!px.is_empty());
    assert!(json.contains("not ready: "), "{json}");
    assert!(took < Duration::from_secs(10), "took {took:?}");
}
