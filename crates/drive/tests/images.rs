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
    shoot_page("page.html", "#png", extra)
}

/// The same for the page `page`, which has the element `wait` once it has loaded.
fn shoot_page(page: &str, wait: &str, extra: &str) -> (usize, Vec<u32>, String) {
    let dir = std::env::temp_dir().join(format!("bare-drive-img-{}-{}-{}", std::process::id(), page, extra.len()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let steps = dir.join("steps");
    std::fs::write(&steps, format!("open {}/{page}\nexpect css=\"{wait}\" visible\n{extra}\nshot s\n", serve())).unwrap();
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
fn a_picture_opened_as_the_document_is_shown_in_an_img() {
    // A link to a picture (Content-Type image/png) opens a document with one `<img>`, as in Chrome.
    let (w, px, json) = shoot_page("red.png", "img", "");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    let first = px.iter().position(|&p| p != 0xffffff).expect("no picture is drawn");
    assert!(near(px[first + 5], (255, 0, 0)), "{:06x}", px[first + 5]);
}

#[test]
fn the_text_after_the_pictures_is_drawn_and_nothing_waits_for_them() {
    // `expect` does not wait for a picture: the step file with no `shot` ends at once, and the page text shows.
    let (w, px, json) = shoot("expect text=\"after\" visible");
    assert!(json.starts_with("{\"ok\":true"), "{json}");
    assert!((100..120).any(|y| (0..40).any(|x| px[y * w + x] != 0xffffff)), "the text is not drawn");
}

#[test]
fn a_block_picture_is_drawn_in_a_float_in_a_flex_row() {
    // The header logo of Wikipedia: `display: block` pictures sized in em, in a float, in a flex item. A block box had no picture in the list.
    let (w, px, _) = shoot_page("logo.html", "#word", "");
    let ink = |x0: usize, x1: usize, y0: usize, y1: usize| (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| px[y * w + x] != 0xffffff).count();
    // The icon (50 by 50, from `width` and `height`), the wordmark (140 by 22) and the tagline (140 by 11) at the right of it.
    assert!(ink(0, 50, 5, 55) > 100, "no icon");
    assert!(ink(60, 200, 5, 30) > 200, "no wordmark");
    assert!(ink(60, 200, 35, 50) > 100, "no tagline");
}

#[test]
fn the_grid_template_shorthand_sizes_the_columns() {
    // `grid-template: rows / columns` (the page of Wikipedia uses it): a 100 px column, a gap of 24 px, and `minmax(0, 1fr)` for the rest of the 250 px.
    let (w, px, _) = shoot_page("g.html", "#c1", "");
    let at = |x: usize, y: usize| px[y * w + x];
    assert!(near(at(50, 10), (0, 170, 0)), "{:06x}", at(50, 10));
    assert!(near(at(110, 10), (255, 255, 255)) && near(at(130, 15), (0, 0, 170)), "{:06x}", at(130, 15));
    assert!(near(at(245, 15), (0, 0, 170)) && near(at(260, 15), (255, 255, 255)), "{:06x}", at(260, 15));
}

#[test]
fn the_content_of_noscript_is_not_shown() {
    // This browser runs scripts, so `noscript` holds raw text, and its box is hidden: the green bar is on top, and no text is below it.
    let (w, px, _) = shoot_page("n.html", "#d", "");
    assert!(near(px[5 * w + 5], (0, 170, 0)), "{:06x}", px[5 * w + 5]);
    assert!((10..120).all(|y| (0..300).all(|x| px[y * w + x] == 0xffffff)), "text of the noscript is drawn");
}

#[test]
fn a_page_style_does_not_show_noscript() {
    // crates.io sets `noscript { display: grid }`. Scripts run, so the box stays hidden: the green bar is on top, and nothing is below it.
    let (w, px, _) = shoot_page("n2.html", "#d", "");
    assert!(near(px[5 * w + 5], (0, 170, 0)), "{:06x}", px[5 * w + 5]);
    assert!((10..120).all(|y| (0..300).all(|x| px[y * w + x] == 0xffffff)), "the noscript box is drawn");
}

#[test]
fn a_script_loads_a_picture_with_the_image_constructor() {
    // `new Image(5, 6)`, `src`, `onload`, `onerror` and `decode()`; the picture is not in the tree. The log is sorted: the order is the network's.
    let (_, _, json) = shoot_page("ctor.html", "#r", "expect css=\"#r\" text=\"bad:error:true,decode:ok,ok:20x10:true:5\"");
    assert!(json.contains("\"ok\":true"), "{json}");
}
