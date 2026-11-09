//! `<img>` through `bare-drive` (`tools/fixtures/img/page.html`): PNG, JPEG, WebP, SVG and `data:` pictures, a missing file, a `<picture>`, and the size rules
//! (natural size, one side set, `max-width`). The server is on loopback and answers from the fixture folder.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Serve `tools/fixtures/img` on a free port. Gives the origin.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
                let file = root().join("tools/fixtures/img").join(&path);
                let (status, kind, body) = match std::fs::read(&file) {
                    Ok(b) if !path.contains("..") => {
                        let kind = match path.rsplit('.').next() {
                            Some("html") => "text/html",
                            Some("svg") => "image/svg+xml",
                            _ => "application/octet-stream",
                        };
                        (200, kind, b)
                    }
                    _ => (404, "text/plain", b"no".to_vec()),
                };
                let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&body));
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Run a step file against the page. Gives the pixels of the capture (width, rows of `0xRRGGBB`), and the JSON.
fn shoot(extra: &str) -> (usize, Vec<u32>, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-img-{}-{}", std::process::id(), extra.len()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let steps = dir.join("steps");
    std::fs::write(&steps, format!("open {}/page.html\nexpect css=\"#png\" visible\n{extra}\nshot s\n", serve())).unwrap();
    let out = dir.join("out");
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_bare-drive")).args(["--quiet", "--size", "300x120", "--out", out.to_str().unwrap()]).arg(&steps).output().unwrap();
    let json = String::from_utf8_lossy(&run.stdout).to_string();
    assert_eq!(run.status.code(), Some(0), "{json} {}", String::from_utf8_lossy(&run.stderr));
    let mut r = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(out.join("s.png")).unwrap())).read_info().unwrap();
    let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
    let f = r.next_frame(&mut buf).unwrap();
    let px = buf[..f.buffer_size()].chunks_exact(3).map(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])).collect();
    std::fs::remove_dir_all(dir).unwrap();
    (f.width as usize, px, json)
}

fn near(p: u32, (r, g, b): (u32, u32, u32)) -> bool {
    let d = |a: u32, b: u32| a.abs_diff(b) < 12;
    d(p >> 16 & 255, r) && d(p >> 8 & 255, g) && d(p & 255, b)
}

#[test]
fn every_format_is_drawn_at_the_size_that_chrome_gives() {
    let (w, px, _) = shoot("");
    let at = |x: usize, y: usize| px[y * w + x];
    let (red, blue, green, yellow, white) = ((255, 0, 0), (0, 0, 255), (0, 255, 0), (255, 255, 0), (255, 255, 255));
    // PNG at its own size, 20 by 10.
    assert!(near(at(1, 1), red) && near(at(18, 8), red), "{:06x}", at(1, 1));
    assert!(near(at(21, 5), white) && near(at(5, 11), white));
    // JPEG with `width: 40px`: the height is 20 by the ratio.
    assert!(near(at(1, 21), blue) && near(at(38, 38), blue), "{:06x}", at(1, 21));
    assert!(near(at(41, 30), white) && near(at(5, 41), white));
    // WebP with `height=30`: the width is 60.
    assert!(near(at(1, 51), green) && near(at(58, 78), green), "{:06x}", at(1, 51));
    assert!(near(at(62, 60), white) && near(at(5, 81), white));
    // SVG at its own size, 24 by 12.
    assert!(near(at(101, 1), yellow) && near(at(122, 10), yellow), "{:06x}", at(101, 1));
    assert!(near(at(126, 5), white) && near(at(110, 13), white));
    // A `data:` PNG, 20 by 10.
    assert!(near(at(101, 21), red) && near(at(118, 28), red), "{:06x}", at(101, 21));
    // `max-width: 30px` wins over `width: 100px`; the height follows the ratio of the picture (15).
    assert!(near(at(101, 51), red) && near(at(128, 63), red), "{:06x}", at(101, 51));
    assert!(near(at(131, 55), white) && near(at(110, 66), white));
    // A picture that is missing: the box of its `width` and `height` and its background stay.
    assert!(near(at(205, 5), (0, 255, 255)), "{:06x}", at(205, 5));
    // The `<source>` of a type that is supported wins over `src`; the one that is not supported does not.
    assert!(near(at(201, 21), green), "{:06x}", at(201, 21));
    assert!(near(at(201, 51), blue), "{:06x}", at(201, 51));
}

#[test]
fn the_text_after_the_pictures_is_drawn_and_nothing_waits_for_them() {
    // `expect` does not wait for a picture: the step file with no `shot` ends at once, and the page text shows.
    let (w, px, json) = shoot("expect text=\"after\" visible");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    assert!((100..120).any(|y| (0..40).any(|x| px[y * w + x] != 0xffffff)), "the text is not drawn");
}
