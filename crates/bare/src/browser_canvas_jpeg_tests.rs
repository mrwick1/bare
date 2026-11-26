//! Tests of `toDataURL` and `toBlob` with `image/jpeg` (`browser_canvas.rs`, `bare_paint::jpeg`). JPEG loses detail, so the tests
//! check the size and close pixels. The reference values come from Chrome (headless shell 1254) for the same script.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Run `body` (a function body that gives a string or a promise of one), then the tasks, and give the result.
fn run(body: &str) -> String {
    let body = body.to_string();
    big(move || {
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.use_virtual_clock();
        b.run_script(&format!("function cv(w, h) {{ var c = document.createElement('canvas'); c.width = w; c.height = h; return c }}
            var out = 'pending'; Promise.resolve((function () {{ {body} }})()).then(function (v) {{ out = String(v) }}, function (e) {{ out = 'throw:' + e.name + ':' + e.message }})")).expect("script");
        b.run_scheduled_tasks(10).unwrap();
        let v = b.run_script("out").expect("out");
        b.it.display(v)
    })
}

fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".bytes().position(|x| x == c).expect("base64") as u32;
    let mut out = Vec::new();
    for q in s.trim_end_matches('=').as_bytes().chunks(4) {
        let n = q.iter().enumerate().fold(0, |n, (i, &c)| n | val(c) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..q.len()]);
    }
    out
}

/// Each channel of `got` (0xAARRGGBB) is within `tol` of `want` (r, g, b), and the alpha is 255.
fn close(got: u32, want: [u8; 3], tol: i32) -> bool {
    let [a, r, g, b] = got.to_be_bytes();
    a == 255 && [r, g, b].iter().zip(want).all(|(&x, y)| (x as i32 - y as i32).abs() <= tol)
}

#[test]
fn a_jpeg_data_url_decodes_to_the_same_size_and_close_pixels() {
    // Chrome: the left pixel reads 129,0,0 and the right one 0,0,2. JPEG has no alpha: the pixels are put on black.
    let url = run("var c = cv(16, 16), x = c.getContext('2d'); x.fillStyle = 'rgba(255,0,0,0.5)'; x.fillRect(0, 0, 8, 16); return c.toDataURL('image/jpeg')");
    let b64 = url.strip_prefix("data:image/jpeg;base64,").expect("a JPEG data URL");
    let img = bare_image::decode(&unbase64(b64)).expect("a JPEG");
    assert_eq!((img.width, img.height), (16, 16));
    assert!(close(img.pixels[2 * 16 + 2], [128, 0, 0], 8), "{:08x}", img.pixels[2 * 16 + 2]);
    assert!(close(img.pixels[2 * 16 + 12], [0, 0, 0], 8), "{:08x}", img.pixels[2 * 16 + 12]);
}

#[test]
fn the_jpeg_quality_defaults_to_0_92() {
    // Chrome: true,true,true,true. A quality out of 0..1 or not a number gives the default.
    let got = run("var c = cv(32, 32), x = c.getContext('2d'), d = x.createImageData(32, 32);
        for (var i = 0; i < d.data.length; i++) d.data[i] = (i * 7919) % 251; x.putImageData(d, 0, 0);
        var u = c.toDataURL('image/jpeg'), lo = c.toDataURL('image/jpeg', 0.1), hi = c.toDataURL('image/jpeg', 1);
        return [u === c.toDataURL('image/jpeg', 0.92), u === c.toDataURL('image/jpeg', 2), u === c.toDataURL('IMAGE/JPEG', '0.5'), lo.length < u.length && u.length < hi.length].join()");
    assert_eq!(got, "true,true,true,true");
}

#[test]
fn a_photo_send_gives_a_jpeg_blob_with_close_pixels() {
    // The WhatsApp case. Chrome: "image/jpeg 4x3 237,164,0,36,59,100"; the drawn pixels are 240,160,0 and 32,64,96.
    let got = run("var s = cv(8, 6), y = s.getContext('2d'); y.fillStyle = '#204060'; y.fillRect(0, 0, 8, 6); y.fillStyle = '#f0a000'; y.fillRect(0, 0, 4, 6);
        return new Promise(function (res) { s.toBlob(res) }).then(function (blob) { return createImageBitmap(blob) }).then(function (bm) {
          var c = cv(4, 3), x = c.getContext('2d'); x.drawImage(bm, 0, 0, 4, 3);
          return new Promise(function (res) { c.toBlob(res, 'image/jpeg', 0.8) }) }).then(function (out) { return createImageBitmap(out).then(function (bm) {
          var c = cv(4, 3), x = c.getContext('2d'); x.drawImage(bm, 0, 0); var d = x.getImageData(0, 0, 4, 1).data;
          return [out.type, bm.width + 'x' + bm.height, d[0], d[1], d[2], d[12], d[13], d[14]].join() }) })");
    let p: Vec<&str> = got.split(',').collect();
    assert_eq!(p[..2], ["image/jpeg", "4x3"], "{got}");
    let n: Vec<i32> = p[2..].iter().map(|v| v.parse().unwrap()).collect();
    let want = [240, 160, 0, 32, 64, 96];
    assert!(n.iter().zip(want).all(|(a, b)| (a - b).abs() <= 12), "{got}");
}
