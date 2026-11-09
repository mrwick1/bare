//! `background-image` and `mask-image` through `bare-drive` (`tools/fixtures/bg/page.html`): the pictures are named by a linked sheet in `css/`
//! (relative to the sheet, not to the page) and by a `data:` URL. The server is on loopback and answers from the fixture folder.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Serve `tools/fixtures/bg` on a free port. Gives the origin.
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
                let file = root().join("tools/fixtures/bg").join(&path);
                let (status, kind, body) = match std::fs::read(&file) {
                    Ok(b) if !path.contains("..") => {
                        let kind = match path.rsplit('.').next() {
                            Some("html") => "text/html",
                            Some("svg") => "image/svg+xml",
                            Some("css") => "text/css",
                            Some("png") => "image/png",
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
    let dir = std::env::temp_dir().join(format!("bare-drive-bg-{}-{}", std::process::id(), extra.len()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let steps = dir.join("steps");
    std::fs::write(&steps, format!("open {}/page.html\nexpect css=\"#a\" visible\n{extra}\nshot s\n", serve())).unwrap();
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

fn at(px: &[u32], w: usize, x: usize, y: usize) -> u32 {
    px[y * w + x]
}

#[test]
fn background_images_and_masks_are_drawn_from_the_sheet_and_from_data_urls() {
    let (w, px, json) = shoot("");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    let p = |x, y| at(&px, w, x, y);
    let (red, blue, green, yellow, white, magenta) = ((255, 0, 0), (0, 0, 255), (0, 255, 0), (255, 255, 0), (255, 255, 255), (255, 0, 255));
    // a: `background: #0f0 url(tile.png) no-repeat`: the 4 by 2 picture at the corner, the color around it.
    assert!(near(p(1, 1), red) && near(p(3, 1), blue) && near(p(10, 10), green), "{:06x} {:06x} {:06x}", p(1, 1), p(3, 1), p(10, 10));
    // b: `repeat-x` with `background-size: 8px 4px`: red 4 pixels, blue 4 pixels, again; only the first 4 rows.
    assert!(near(p(51, 1), red) && near(p(55, 1), blue) && near(p(59, 3), red) && near(p(79, 2), blue), "{:06x} {:06x} {:06x}", p(51, 1), p(55, 1), p(59, 3));
    assert!(near(p(51, 5), white) && near(p(70, 15), white), "{:06x}", p(51, 5));
    // c: `right bottom / 20px 10px no-repeat` over yellow: the picture is in the lower right quarter-ish, 20 by 10.
    assert!(near(p(121, 12), red) && near(p(135, 12), blue) && near(p(105, 5), yellow) && near(p(105, 15), yellow), "{:06x} {:06x} {:06x}", p(121, 12), p(135, 12), p(105, 5));
    // d: a red box through a mask whose left half is opaque.
    assert!(near(p(152, 10), red) && near(p(167, 10), white), "{:06x} {:06x}", p(152, 10), p(167, 10));
    // e: the `mask` shorthand, 10 by 20 at the right edge: the opaque half is 5 pixels wide.
    assert!(near(p(191, 10), blue) && near(p(197, 10), white) && near(p(183, 10), white), "{:06x} {:06x} {:06x}", p(191, 10), p(197, 10), p(183, 10));
    // f: a mask that is not found hides the box.
    assert!(near(p(215, 10), white), "{:06x}", p(215, 10));
    // g: a `data:` URL in a `style` attribute: the top half of the box.
    assert!(near(p(245, 5), magenta) && near(p(245, 15), white), "{:06x} {:06x}", p(245, 5), p(245, 15));
}

#[test]
fn the_text_after_the_boxes_is_drawn_and_nothing_waits_for_the_pictures() {
    let (w, px, json) = shoot("expect text=\"after\" visible");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    assert!((100..120).any(|y| (0..40).any(|x| at(&px, w, x, y) != 0xffffff)), "the text is not drawn");
}
